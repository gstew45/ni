//! The listener half of a bot: bind what the engine asked for, announce it,
//! serve until `SIGTERM`.
//!
//! From M5 there are two kinds of "what the engine asked for", and the
//! difference between them is almost entirely about *names*. A TCP listener
//! borrows a port from an allocator and gives it back on close. A Unix
//! listener creates a file, and a file has to be created carefully, protected
//! deliberately, and deleted by somebody.

use std::{
    io::Write as _,
    path::{Path, PathBuf},
};

use anyhow::{bail, Context, Result};
use ni_proto::ni::v1::bot_service_server::{BotService, BotServiceServer};
use tokio::{
    net::{TcpListener, UnixListener, UnixStream},
    signal::unix::{signal, SignalKind},
};
use tokio_stream::wrappers::{TcpListenerStream, UnixListenerStream};
use tonic::{transport::server::UdsConnectInfo, transport::Server, Request};

pub async fn serve<S>(listen: &str, service: S) -> Result<()>
where
    S: BotService,
{
    if let Some(address) = listen.strip_prefix("tcp://") {
        serve_tcp(address, service).await
    } else if let Some(path) = listen.strip_prefix("unix://") {
        serve_unix(Path::new(path), service).await
    } else {
        bail!("listen target must begin with tcp:// or unix://, got {listen:?}")
    }
}

async fn serve_tcp<S>(bind_address: &str, service: S) -> Result<()>
where
    S: BotService,
{
    let listener = TcpListener::bind(bind_address).await?;
    let local_address = listener.local_addr()?;

    announce(&format!("tcp://{local_address}"))?;

    Server::builder()
        .add_service(BotServiceServer::new(service))
        .serve_with_incoming_shutdown(TcpListenerStream::new(listener), terminated())
        .await?;

    Ok(())
}

async fn serve_unix<S>(path: &Path, service: S) -> Result<()>
where
    S: BotService,
{
    clear_stale_socket(path).await?;

    let listener = UnixListener::bind(path)
        .with_context(|| format!("could not bind unix://{}", path.display()))?;

    // `bind` created the file with `0666 & ~umask`, whatever the umask
    // happens to be. Narrow it explicitly. Linux enforces this on `connect`;
    // some other Unixes do not, which is why the engine also locks the
    // containing directory.
    restrict_to_owner(path)?;

    // The guard, not the listener, owns the name in the filesystem. Closing a
    // Unix listener does not remove its socket file — the next bind would then
    // fail with EADDRINUSE against a socket nobody is listening on.
    let _socket_file = SocketFile(path.to_path_buf());

    announce(&format!("unix://{}", path.display()))?;

    Server::builder()
        .add_service(BotServiceServer::new(service))
        .serve_with_incoming_shutdown(UnixListenerStream::new(listener), terminated())
        .await?;

    Ok(())
}

/// The readiness line the engine parses, on stdout, flushed.
///
/// It carries the scheme as well as the address, so the engine dials what the
/// bot actually bound rather than what it hoped for.
fn announce(endpoint: &str) -> Result<()> {
    println!("LISTENING {endpoint}");
    std::io::stdout().flush()?;
    Ok(())
}

/// Who is on the other end of this call?
///
/// This is the one place where the two transports genuinely differ in what
/// they can *tell* you, and it is the strongest argument for UDS that has
/// nothing to do with speed.
///
/// * Over TCP you get an address and a port. `127.0.0.1` proves the packet
///   came from this host and nothing more — any process, any user, any
///   container sharing the network namespace could have sent it.
/// * Over a Unix socket the kernel attaches the peer's pid, uid and gid to the
///   connection (`SO_PEERCRED`). The peer cannot lie about them: it never
///   supplies them, the kernel does, from the process it knows made the
///   `connect` call.
pub fn describe_peer<T>(request: &Request<T>) -> String {
    if let Some(info) = request.extensions().get::<UdsConnectInfo>() {
        return match info.peer_cred {
            Some(credentials) => format!(
                "unix pid={} uid={} gid={}",
                credentials
                    .pid()
                    .map(|pid| pid.to_string())
                    .unwrap_or_else(|| "?".to_string()),
                credentials.uid(),
                credentials.gid()
            ),
            None => "unix (credentials unavailable)".to_string(),
        };
    }

    match request.remote_addr() {
        Some(address) => format!("tcp {address}"),
        None => "unknown".to_string(),
    }
}

/// A socket file that unlinks itself.
struct SocketFile(PathBuf);

impl Drop for SocketFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Remove a socket file left behind by a dead process — and only that.
///
/// The naive version (`let _ = remove_file(path)`) is a footgun: run two bots
/// on one path and the second silently steals the name, leaving the first
/// holding a socket no client can reach. So: try to connect first. Somebody
/// answering means the socket is live and this is a configuration error;
/// `ECONNREFUSED` means the file outlived its process and can go.
async fn clear_stale_socket(path: &Path) -> Result<()> {
    if !path.exists() {
        return Ok(());
    }

    match UnixStream::connect(path).await {
        Ok(_) => bail!(
            "another process is already listening on unix://{}",
            path.display()
        ),
        Err(error) if error.kind() == std::io::ErrorKind::ConnectionRefused => {
            tracing::warn!(path = %path.display(), "removing a stale socket file");
            std::fs::remove_file(path)
                .with_context(|| format!("could not remove stale socket {}", path.display()))
        }
        Err(error) => Err(error)
            .with_context(|| format!("could not probe existing socket {}", path.display())),
    }
}

fn restrict_to_owner(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt as _;

    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("could not restrict {} to its owner", path.display()))
}

async fn terminated() {
    match signal(SignalKind::terminate()) {
        Ok(mut sigterm) => {
            sigterm.recv().await;
            tracing::info!("SIGTERM: draining and flushing telemetry");
        }
        Err(error) => {
            tracing::warn!(%error, "cannot listen for SIGTERM; running until killed");
            std::future::pending::<()>().await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_missing_socket_needs_no_clearing() {
        let path = std::env::temp_dir().join("ni-m5-absent.sock");
        let _ = std::fs::remove_file(&path);

        assert!(clear_stale_socket(&path).await.is_ok());
    }

    #[tokio::test]
    async fn a_stale_socket_file_is_removed() {
        let path = std::env::temp_dir().join("ni-m5-stale.sock");
        let _ = std::fs::remove_file(&path);

        // Bind and drop: the listener is gone, the name is not.
        {
            let _listener = UnixListener::bind(&path).expect("bind");
        }
        assert!(path.exists(), "closing a listener leaves the file behind");

        clear_stale_socket(&path)
            .await
            .expect("stale file is cleared");
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn a_live_socket_is_left_alone_and_reported() {
        let path = std::env::temp_dir().join("ni-m5-live.sock");
        let _ = std::fs::remove_file(&path);

        let _listener = UnixListener::bind(&path).expect("bind");

        let error = clear_stale_socket(&path)
            .await
            .expect_err("a live socket is not ours to delete");

        assert!(error.to_string().contains("already listening"));
        assert!(path.exists());

        let _ = std::fs::remove_file(&path);
    }
}
