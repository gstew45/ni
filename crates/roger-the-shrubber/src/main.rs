use anyhow::Result;
use clap::Parser;
use ni_bot::serve;
use roger_the_shrubber::{Mischief, Roger};

#[derive(Parser)]
#[command(about = "Run roger-the-shrubber: A deliberately hostile Ni bot")]
struct Cli {
    #[arg(long, default_value = "tcp://127.0.0.1:0")]
    listen: String,

    #[command(flatten)]
    mischief: Mischief,
}

#[tokio::main]
async fn main() -> Result<()> {
    let telemetry = ni_telemetry::init("roger-the-shrubber");

    let cli = Cli::parse();

    tracing::info!(mischief = ?cli.mischief, "roger reporting for duty");

    let result = serve(&cli.listen, Roger::new(cli.mischief)).await;

    telemetry.shutdown();
    result
}
