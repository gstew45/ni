use anyhow::Result;
use clap::Parser;
use ni_bot::{serve, ReferenceBot};

#[derive(Parser)]
#[command(about = "Run the Ni reference bot gRPC server")]
struct Cli {
    #[arg(long, default_value = "tcp://127.0.0.1:0")]
    listen: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    serve(&cli.listen, ReferenceBot::default()).await
}
