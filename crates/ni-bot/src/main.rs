use std::io::Write as _;

use anyhow::{Context, Result};
use clap::Parser;
use ni_bot::ReferenceBot;
use ni_proto::ni::v1::bot_service_server::BotServiceServer;
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;

#[derive(Parser)]
#[command(about = "Run the Ni reference bot gRPC server")]
struct Cli {
    #[arg(long, default_value = "tcp://127.0.0.1:0")]
    listen: String,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    let bind_address = cli
        .listen
        .strip_prefix("tcp://")
        .context("M2 listen target must begin with tcp://")?;

    let listener = TcpListener::bind(bind_address).await?;
    let local_address = listener.local_addr()?;

    println!("LISTENING tcp://{local_address}");
    std::io::stdout().flush()?;

    Server::builder()
        .add_service(BotServiceServer::new(ReferenceBot::default()))
        .serve_with_incoming(TcpListenerStream::new(listener))
        .await?;

    Ok(())
}
