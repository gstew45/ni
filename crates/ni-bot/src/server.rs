use std::io::Write as _;

use anyhow::{Context, Result};
use ni_proto::ni::v1::bot_service_server::{BotService, BotServiceServer};
use tokio::{
    net::TcpListener,
    signal::unix::{signal, SignalKind},
};
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;

pub async fn serve<S>(listen: &str, service: S) -> Result<()>
where
    S: BotService,
{
    let bind_address = listen
        .strip_prefix("tcp://")
        .context("listen target must begin with tcp://")?;

    let listener = TcpListener::bind(bind_address).await?;
    let local_address = listener.local_addr()?;

    println!("LISTENING tcp://{local_address}");
    std::io::stdout().flush()?;

    Server::builder()
        .add_service(BotServiceServer::new(service))
        .serve_with_incoming_shutdown(TcpListenerStream::new(listener), terminated())
        .await?;

    Ok(())
}

async fn terminated() {
    match signal(SignalKind::terminate()) {
        Ok(mut sigterm) => {
            sigterm.recv().await;
            tracing::info!("SIGTERM: draining and flushing telemetry");
        }
        Err(error) => {
            tracing::warn!(%error, "cannot listen for SIGTERM; running until killed");
            std::future::pending::<()>().await;
        }
    }
}
