use std::path::PathBuf;

use anyhow::{ensure, Result};
use clap::{Parser, Subcommand};
use ni_engine::BotProcess;
use ni_proto::{ni::v1::IdentifyRequest, PROTOCOL_VERSION};

#[derive(Parser)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Run {
        #[arg(long)]
        bot_a: PathBuf,
        #[arg(long)]
        bot_b: PathBuf,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Run { bot_a, bot_b } => run(bot_a, bot_b).await,
    }
}

async fn run(bot_a_path: PathBuf, bot_b_path: PathBuf) -> Result<()> {
    let mut bot_a = BotProcess::spawn(&bot_a_path, "bot A").await?;

    let mut bot_b = match BotProcess::spawn(&bot_b_path, "bot B").await {
        Ok(bot) => bot,
        Err(error) => {
            let _ = bot_a.shutdown().await;
            return Err(error);
        }
    };

    let work: Result<()> = async {
        identify(&mut bot_a, "A").await?;
        identify(&mut bot_b, "B").await?;
        Ok(())
    }
    .await;

    let cleanup_a = bot_a.shutdown().await;
    let cleanup_b = bot_b.shutdown().await;

    work?;
    cleanup_a?;
    cleanup_b?;
    Ok(())
}

async fn identify(bot: &mut BotProcess, label: &str) -> Result<()> {
    let identity = bot
        .client
        .identify(IdentifyRequest {
            protocol_version: PROTOCOL_VERSION,
        })
        .await?
        .into_inner();

    ensure!(
        identity.protocol_version == PROTOCOL_VERSION,
        "bot {label} speaks protocol {}, engine requires {}",
        identity.protocol_version,
        PROTOCOL_VERSION,
    );

    println!(
        "bot {label}: pid={}, tcp://{}, {} {}",
        bot.id()
            .map(|id| id.to_string())
            .unwrap_or_else(|| "unknown".to_string()),
        bot.endpoint(),
        identity.name,
        identity.version,
    );

    Ok(())
}
