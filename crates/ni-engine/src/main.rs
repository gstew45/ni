use std::{path::PathBuf, time::Duration};

use anyhow::Result;
use clap::{Parser, Subcommand};
use ni_engine::{run_match, BotProcess, MatchConclusion, RunOptions, TimeControl};

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

        #[arg(long = "bot-a-arg", allow_hyphen_values = true)]
        bot_a_arg: Vec<String>,

        #[arg(long = "bot-b-arg", allow_hyphen_values = true)]
        bot_b_arg: Vec<String>,

        #[arg(
            long,
            default_value= "0ms",
            value_parser = parse_duration
        )]
        delay: Duration,

        #[arg(
            long,
            default_value = "500ms",
            value_parser = parse_duration
        )]
        turn_deadline: Duration,

        #[arg(long, default_value_t = 3)]
        strike_limit: u32,

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
    let Command::Run {
        bot_a,
        bot_b,
        bot_a_arg,
        bot_b_arg,
        delay,
        turn_deadline,
        strike_limit,
        quiet,
    } = Cli::parse().command;

    let options = RunOptions {
        delay,
        quiet,
        match_id: "m3-demo".to_string(),
        time: TimeControl {
            turn_deadline,
            strike_limit,
        },
    };

    let conclusion = run(bot_a, bot_a_arg, bot_b, bot_b_arg, options).await?;

    if let MatchConclusion::Forfeit { loser, reason } = conclusion {
        eprintln!("note: chapter {loser:?} forfeited ({reason:?})");
    }

    Ok(())
}

async fn run(
    bot_a_path: PathBuf,
    bot_a_args: Vec<String>,
    bot_b_path: PathBuf,
    bot_b_args: Vec<String>,
    options: RunOptions,
) -> Result<MatchConclusion> {
    let mut bot_a = BotProcess::spawn_with_args(&bot_a_path, "bot A", &bot_a_args).await?;

    let mut bot_b = match BotProcess::spawn_with_args(&bot_b_path, "bot B", &bot_b_args).await {
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

    let match_result = run_match(&mut bot_a, &mut bot_b, options).await;

    let cleanup_a = bot_a.shutdown().await;
    let cleanup_b = bot_b.shutdown().await;

    let conclusion = match_result?;
    cleanup_a?;
    cleanup_b?;
    Ok(conclusion)
}
