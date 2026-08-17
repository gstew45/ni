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
    let cli = Cli::parse();

    eprintln!("roger: {:?}", cli.mischief);

    serve(&cli.listen, Roger::new(cli.mischief)).await
}
