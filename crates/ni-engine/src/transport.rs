//! Where a bot listens, and how the engine dials it.
//!
//! M5's whole surface area. Two transports, one service definition:
//!
//! * **loopback TCP** — `tcp://127.0.0.1:0`, the kernel picks a port, the bot
//!   prints the one it got.
//! * **Unix domain socket** — `unix:///run/ni/<match>/a.sock`, the *engine*
//!   picks the path, because a path is not a port: nothing allocates one for
//!   you, and nothing cleans one up either.
//!
//! Everything above this module — deadlines, statuses, spans, the match loop —
//! is written against `BotServiceClient<Channel>` and does not know which of
//! the two it got. That is the claim M5 exists to test.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use anyhow::{bail, Context, Result};
use hyper_util::rt::TokioIo;
use ni_proto::ni::v1::bot_service_client::BotServiceClient;
use tokio::net::UnixStream;
use tonic::transport::{Channel, Endpoint, Uri};
use tower::service_fn;

/// `sockaddr_un.sun_path` is a fixed-size array: 108 bytes on Linux, 104 on
/// macOS. A path that does not fit is not truncated — `bind` fails. Ni checks
/// the length itself so the error names the real problem.
pub const SUN_PATH_MAX: usize = 100;

/// Which socket family the match runs over.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum Transport {
    #[default]
    Tcp,
    Unix,
}

impl Transport {
    pub fn as_str(self) -> &'static str {
        match self {
            Transport::Tcp => "tcp",
            Transport::Unix => "unix",
        }
    }
}

impl std::str::FromStr for Transport {
    type Err = String;

    fn from_str(value: &str) -> std::result::Result<Self, Self::Err> {
        match value {
            "tcp" => Ok(Transport::Tcp),
            "unix" | "uds" => Ok(Transport::Unix),
            other => Err(format!("unknown transport {other:?}; expected tcp or unix")),
        }
    }
}

/// The listen target the engine hands a bot on argv.
///
/// The asymmetry is the interesting part. For TCP the engine says "port 0,
/// you tell me"; for UDS the engine says "this exact path". Ports come from
/// an allocator, paths come from whoever is willing to name one.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Listen {
    Tcp,
    Unix(PathBuf),
}

impl Listen {
    /// What goes after `--listen`.
    pub fn argv(&self) -> String {
        match self {
            Listen::Tcp => "tcp://127.0.0.1:0".to_string(),
            // Three slashes: `unix://` + an absolute path that starts with one.
            Listen::Unix(path) => format!("unix://{}", path.display()),
        }
    }
}

/// A directory of bot sockets that deletes itself.
///
/// A TCP port is reclaimed by the kernel when the last socket closes. A socket
/// *file* is not: it is a name in a filesystem, and names outlive processes.
/// Someone has to own the cleanup, and the engine is the only participant that
/// knows when a match is over.
#[derive(Debug)]
pub struct SocketDir {
    path: PathBuf,
}

impl SocketDir {
    /// Create `<base>/<match_id>/`, owner-only.
    ///
    /// `0o700` on the directory is the load-bearing permission. A socket file
    /// created by `bind` gets its mode from the process umask, and on some
    /// Unixes the socket's own mode is not even consulted on `connect` — but
    /// every Unix checks execute permission on the directories along the path.
    /// Lock the directory and the socket inside it is unreachable, whatever
    /// its own mode says.
    pub fn create(base: &Path, match_id: &str) -> Result<Self> {
        let safe: String = match_id
            .chars()
            .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
            .collect();

        let path = base.join(safe);

        std::fs::create_dir_all(&path)
            .with_context(|| format!("could not create socket directory {}", path.display()))?;

        set_owner_only(&path)?;

        Ok(Self { path })
    }

    /// The default socket base: `$XDG_RUNTIME_DIR/ni`, falling back to
    /// `/tmp/ni-<uid>`.
    ///
    /// The design doc says `/run/ni`, which needs root. `$XDG_RUNTIME_DIR` is
    /// the unprivileged equivalent — per-user, already `0700`, and on a tmpfs
    /// that the session manager empties at logout.
    pub fn default_base() -> PathBuf {
        match std::env::var_os("XDG_RUNTIME_DIR") {
            Some(dir) if !dir.is_empty() => PathBuf::from(dir).join("ni"),
            // SAFETY: `getuid` reads a process property and cannot fail.
            _ => PathBuf::from(format!("/tmp/ni-{}", unsafe { libc::getuid() })),
        }
    }

    /// A socket path for one bot, length-checked before anything tries to bind it.
    pub fn socket(&self, label: &str) -> Result<PathBuf> {
        let path = self.path.join(format!("{label}.sock"));
        let length = path.as_os_str().len();

        if length > SUN_PATH_MAX {
            bail!(
                "socket path is {length} bytes, over the {SUN_PATH_MAX}-byte limit: {} \
                 — pass a shorter --socket-dir",
                path.display()
            );
        }

        Ok(path)
    }

    pub fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for SocketDir {
    fn drop(&mut self) {
        // Best effort by definition: a directory we cannot remove is a
        // diagnostic, not a match result.
        if let Err(error) = std::fs::remove_dir_all(&self.path) {
            if error.kind() != std::io::ErrorKind::NotFound {
                tracing::warn!(
                    path = %self.path.display(),
                    %error,
                    "could not remove socket directory"
                );
            }
        }
    }
}

fn set_owner_only(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("could not restrict {} to its owner", path.display()))
}

/// Dial an endpoint the bot printed on its readiness line.
///
/// Both arms end in the same type. That is not a convenience — it is the
/// milestone: `Channel` is tonic's transport-erased handle, so the generated
/// client, the deadlines, the statuses and the metadata are identical either
/// way.
pub async fn dial(endpoint: &str, connect_timeout: Duration) -> Result<BotServiceClient<Channel>> {
    if let Some(address) = endpoint.strip_prefix("tcp://") {
        dial_tcp(address, connect_timeout).await
    } else if let Some(path) = endpoint.strip_prefix("unix://") {
        dial_unix(Path::new(path), connect_timeout).await
    } else {
        bail!("endpoint must begin with tcp:// or unix://, got {endpoint:?}")
    }
}

async fn dial_tcp(address: &str, connect_timeout: Duration) -> Result<BotServiceClient<Channel>> {
    let channel = Endpoint::from_shared(format!("http://{address}"))?
        .connect_timeout(connect_timeout)
        .connect()
        .await
        .with_context(|| format!("could not connect to tcp://{address}"))?;

    Ok(BotServiceClient::new(channel))
}

/// The UDS dance, which looks stranger than it is.
///
/// `Endpoint` insists on a URI because HTTP/2 needs an `:authority`
/// pseudo-header and gRPC puts the service's host there. A Unix socket has no
/// host, so the URI below is a placeholder that is never resolved: the
/// connector ignores it and connects to the path instead. It still has to be
/// syntactically valid, and it still ends up in the `:authority` header the
/// bot receives.
async fn dial_unix(path: &Path, connect_timeout: Duration) -> Result<BotServiceClient<Channel>> {
    let path = path.to_path_buf();
    let target = path.clone();

    let channel = Endpoint::try_from("http://ni.invalid")?
        .connect_timeout(connect_timeout)
        // `service_fn` turns a closure into a `tower::Service`. tonic asks the
        // connector for a connection per `Uri`; this one throws the `Uri` away.
        .connect_with_connector(service_fn(move |_: Uri| {
            let path = path.clone();
            async move {
                // `TokioIo` adapts a tokio `AsyncRead`/`AsyncWrite` to hyper's
                // own IO traits. It is pure plumbing — hyper 1.0 stopped
                // depending on tokio's traits directly, so every custom
                // transport needs this wrapper.
                Ok::<_, std::io::Error>(TokioIo::new(UnixStream::connect(path).await?))
            }
        }))
        .await
        .with_context(|| format!("could not connect to unix://{}", target.display()))?;

    Ok(BotServiceClient::new(channel))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_transport_parses_from_its_flag_value() {
        assert_eq!("tcp".parse::<Transport>(), Ok(Transport::Tcp));
        assert_eq!("unix".parse::<Transport>(), Ok(Transport::Unix));
        assert_eq!("uds".parse::<Transport>(), Ok(Transport::Unix));
        assert!("smoke-signals".parse::<Transport>().is_err());
    }

    #[test]
    fn tcp_asks_the_kernel_for_a_port_and_unix_names_a_path() {
        assert_eq!(Listen::Tcp.argv(), "tcp://127.0.0.1:0");
        assert_eq!(
            Listen::Unix(PathBuf::from("/run/ni/m5/a.sock")).argv(),
            "unix:///run/ni/m5/a.sock"
        );
    }

    #[test]
    fn a_socket_directory_is_owner_only_and_removes_itself() {
        use std::os::unix::fs::PermissionsExt as _;

        let base = std::env::temp_dir().join("ni-m5-socketdir-test");
        let path;

        {
            let dir = SocketDir::create(&base, "m5/demo match").expect("directory is created");
            path = dir.path().to_path_buf();

            assert!(path.is_dir());
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o700
            );
            // The match id is sanitised: one component, no separators.
            assert_eq!(path.file_name().unwrap(), "m5-demo-match");
        }

        assert!(!path.exists(), "dropping the guard removes the directory");
        let _ = std::fs::remove_dir_all(&base);
    }

    #[test]
    fn an_over_long_socket_path_is_refused_before_bind() {
        let base = std::env::temp_dir().join("ni-m5-longpath-test");
        let dir = SocketDir::create(&base, &"x".repeat(120)).expect("directory is created");

        let error = dir.socket("a").expect_err("the path is too long");
        assert!(error.to_string().contains("over the 100-byte limit"));

        let _ = std::fs::remove_dir_all(&base);
    }

    #[tokio::test]
    async fn an_unknown_scheme_is_rejected_without_touching_the_network() {
        let error = dial("ni://somewhere", Duration::from_millis(50))
            .await
            .expect_err("no such scheme");

        assert!(error
            .to_string()
            .contains("must begin with tcp:// or unix://"));
    }
}
