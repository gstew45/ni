//! Spawn, connect to, and reap one bot process.

use std::{path::Path, process::Stdio};

use anyhow::{Context, Result};
use ni_proto::ni::v1::bot_service_client::BotServiceClient;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    task::JoinHandle,
};
use tonic::transport::Channel;

pub struct BotProcess {
    pub client: BotServiceClient<Channel>,
    endpoint: String,
    child: Child,
    stdout_task: JoinHandle<()>,
}

impl BotProcess {
    pub async fn spawn(path: &Path, label: &str) -> Result<Self> {
        let mut child = Command::new(path)
            .arg("--listen")
            .arg("tcp://127.0.0.1:0")
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("failed to spawn {label} from {}", path.display()))?;

        let setup: Result<_> = async {
            let stdout = child.stdout.take().context("bot stdout was not piped")?;

            let mut lines = BufReader::new(stdout).lines();

            let ready = lines
                .next_line()
                .await?
                .context("bot exited before its readiness line")?;

            let address = ready
                .strip_prefix("LISTENING tcp://")
                .context("invalid bot readiness line")?
                .to_string();

            let client = BotServiceClient::connect(format!("http://{address}"))
                .await
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

    pub async fn shutdown(&mut self) -> Result<()> {
        if self.child.try_wait()?.is_none() {
            self.child.kill().await?;
        }

        let _ = self.child.wait().await;
        let _ = (&mut self.stdout_task).await;
        Ok(())
    }
}
