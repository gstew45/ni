//! Spawn, connect to, and reap one bot process.

use std::{ffi::OsStr, path::Path, process::Stdio, time::Duration};

use anyhow::{Context, Result};
use ni_proto::ni::v1::bot_service_client::BotServiceClient;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    task::JoinHandle,
};
use tonic::transport::Channel;

pub const STARTUP_DEADLINE: Duration = Duration::from_secs(5);
/// How long a bot gets to finish its own shutdown — which, from M4 on,
/// includes flushing whatever spans are still sitting in its batch queue.
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(2);

pub struct BotProcess {
    pub client: BotServiceClient<Channel>,
    endpoint: String,
    child: Child,
    stdout_task: JoinHandle<()>,
}

impl BotProcess {
    pub async fn spawn(path: &Path, label: &str) -> Result<Self> {
        Self::spawn_with_args::<&OsStr>(path, label, &[]).await
    }

    pub async fn spawn_with_args<S: AsRef<OsStr>>(
        path: &Path,
        label: &str,
        extra_args: &[S],
    ) -> Result<Self> {
        let mut child = Command::new(path)
            .arg("--listen")
            .arg("tcp://127.0.0.1:0")
            .args(extra_args)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("failed to spawn {label} from {}", path.display()))?;

        let setup: Result<_> = async {
            let stdout = child.stdout.take().context("bot stdout was not piped")?;

            let mut lines = BufReader::new(stdout).lines();

            // tokio::time::timeout wraps any future in a deadline. This one
            // covers the readiness line and the dial together, because both
            // are "is this bot alive yet?".
            let ready = tokio::time::timeout(STARTUP_DEADLINE, lines.next_line())
                .await
                .with_context(|| {
                    format!(
                        "{label} printed no readiness line within {}ms",
                        STARTUP_DEADLINE.as_millis()
                    )
                })?
                .context("failed to read bot stdout")?
                .context("bot exited before its readiness line")?;

            let address = ready
                .strip_prefix("LISTENING tcp://")
                .context("invalid bot readiness line")?
                .to_string();

            let client = tokio::time::timeout(
                STARTUP_DEADLINE,
                BotServiceClient::connect(format!("http://{address}")),
            )
            .await
            .with_context(|| format!("{label} did not accept a connection at {address}"))?
            .with_context(|| format!("failed to connect to {label} at {address}"))?;

            Ok((client, address, lines))
        }
        .await;

        let (client, endpoint, mut lines) = match setup {
            Ok(ready) => ready,
            Err(error) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(error);
            }
        };

        let label = label.to_string();
        let stdout_task = tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                eprintln!("[{label}] {line}");
            }
        });

        Ok(Self {
            client,
            endpoint,
            child,
            stdout_task,
        })
    }

    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    pub fn exit_status(&mut self) -> Result<Option<i32>> {
        match self.child.try_wait()? {
            Some(status) => Ok(Some(status.code().unwrap_or(-1))),
            None => Ok(None),
        }
    }

    /// Ask, wait, then insist.
    ///
    /// M3 killed bots outright. That was fine when a bot had nothing to say
    /// on the way out; a bot that exports telemetry has a batch queue, and
    /// `SIGKILL` throws it away. So: `SIGTERM` first, a short grace period,
    /// and `SIGKILL` only for a bot that ignores it.
    pub async fn shutdown(&mut self) -> Result<()> {
        if self.child.try_wait()?.is_none() {
            self.request_termination();

            if tokio::time::timeout(SHUTDOWN_GRACE, self.child.wait())
                .await
                .is_err()
            {
                tracing::warn!(
                    grace_ms = SHUTDOWN_GRACE.as_millis(),
                    "bot ignored SIGTERM; killing it"
                );
                let _ = self.child.kill().await;
            }
        }

        let _ = self.child.wait().await;
        let _ = (&mut self.stdout_task).await;
        Ok(())
    }

    /// `tokio::process::Child` can only `SIGKILL`, so the polite signal goes
    /// through `libc`. The pid belongs to a child we spawned and have not
    /// reaped, so it cannot have been recycled.
    fn request_termination(&mut self) {
        let Some(pid) = self.child.id() else {
            return;
        };

        // SAFETY: `kill` with a pid we own and a valid signal number.
        unsafe {
            libc::kill(pid as libc::pid_t, libc::SIGTERM);
        }
    }
}
