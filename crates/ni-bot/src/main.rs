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
    // The engine spawns this process, so it inherits the engine's
    // OTEL_EXPORTER_OTLP_ENDPOINT — but the service name is per-binary.
    let telemetry = ni_telemetry::init("reference-bot");

    let cli = Cli::parse();
    let result = serve(&cli.listen, ReferenceBot::default()).await;

    telemetry.shutdown();
    result
}
