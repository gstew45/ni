use std::{path::PathBuf, time::Duration};

use anyhow::Result;
use clap::{Parser, Subcommand};
use ni_engine::{run_match, BotProcess, RunOptions};

#[derive(Parser)]
#[command(about = "Run authoritative Ni matches")]
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

        #[arg(
            long,
            default_value= "0ms",
            value_parser = parse_duration
        )]
        delay: Duration,

        #[arg(long)]
        quiet: bool,
    },
}

fn parse_duration(value: &str) -> std::result::Result<Duration, String> {
    let milliseconds = value
        .strip_suffix("ms")
        .ok_or_else(|| "duration mus end in ms, for example 200ms".to_string())?
        .parse::<u64>()
        .map_err(|error| format!("invalid millisecond duration: {error}"))?;

    Ok(Duration::from_millis(milliseconds))
}

#[tokio::main]
async fn main() -> Result<()> {
    match Cli::parse().command {
        Command::Run {
            bot_a,
            bot_b,
            delay,
            quiet,
        } => run(bot_a, bot_b, delay, quiet).await,
    }
}

async fn run(bot_a_path: PathBuf, bot_b_path: PathBuf, delay: Duration, quiet: bool) -> Result<()> {
    let mut bot_a = BotProcess::spawn(&bot_a_path, "bot A").await?;

    let mut bot_b = match BotProcess::spawn(&bot_b_path, "bot B").await {
        Ok(bot) => bot,
        Err(error) => {
            if let Err(cleanup_error) = bot_a.shutdown().await {
                eprintln!(
                    "could not clean up bot A after bot B failed to start: \
                     {cleanup_error}"
                );
            }
            return Err(error);
        }
    };

    let match_result = run_match(
        &mut bot_a,
        &mut bot_b,
        RunOptions {
            delay,
            quiet,
            match_id: "m2-demo".to_string(),
        },
    )
    .await;

    let cleanup_a = bot_a.shutdown().await;
    let cleanup_b = bot_b.shutdown().await;

    match_result?;
    cleanup_a?;
    cleanup_b?;
    Ok(())
}
