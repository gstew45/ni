//! What "nobody is there" looks like on each transport, before gRPC starts.
use std::time::Duration;

use ni_engine::transport::dial;

#[tokio::main]
async fn main() {
    let missing = std::env::temp_dir().join("ni-missing.sock");
    let _ = std::fs::remove_file(&missing);

    let stale = std::env::temp_dir().join("ni-stale.sock");
    let _ = std::fs::remove_file(&stale);
    {
        let _listener = tokio::net::UnixListener::bind(&stale).expect("bind");
    }

    for endpoint in [
        format!("unix://{}", missing.display()),
        format!("unix://{}", stale.display()),
        "tcp://127.0.0.1:1".to_string(),
    ] {
        match dial(&endpoint, Duration::from_millis(500)).await {
            Ok(_) => println!("{endpoint}: connected (unexpected)"),
            Err(error) => println!("{endpoint}\n    {error:#}"),
        }
    }

    let _ = std::fs::remove_file(&stale);
}
