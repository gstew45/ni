//! `ni-bench` — the same service definition, over two transports.
//!
//! Post 6's whole argument fits in this binary: take one gRPC contract, run it
//! over loopback TCP and over a Unix domain socket, and see which costs belong
//! to the wire and which belong to the boundary.
//!
//! It deliberately reuses the *real* RPCs rather than a synthetic echo method:
//!
//! | case | RPC | what it measures |
//! |---|---|---|
//! | `Identify` | `Identify` | the floor: a call with almost no payload |
//! | `GetOrders` | `GetOrders` | the hot path, at its real size |
//! | `SubmitReplay 1KiB` | `SubmitReplay` | small payload |
//! | `SubmitReplay 1MiB` | `SubmitReplay` | large payload |
//!
//! Nothing here is a microbenchmark of protobuf or of tokio. Every number is a
//! full client-observed round trip: encode, write, read, decode, on both sides.

use std::{path::PathBuf, time::Instant};

use anyhow::{Context, Result};
use clap::Parser;
use ni_bench::{replay_of_at_least, Samples, Summary};
use ni_engine::{
    convert::{battlefield_view, new_match_request},
    BotProcess, Listen, SocketDir, TimeControl, Transport,
};
use ni_proto::{
    ni::v1::{GetOrdersRequest, IdentifyRequest, Replay, SubmitReplayRequest},
    PROTOCOL_VERSION,
};
use prost::Message as _;
use tonic::Request;

/// Generous on purpose. A deadline that fires mid-benchmark turns a latency
/// measurement into a timeout measurement.
const CALL_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

#[derive(Parser)]
#[command(about = "Measure one Ni service definition over loopback TCP and Unix sockets")]
struct Cli {
    /// Bot binary to measure against.
    #[arg(long, default_value = "target/debug/ni-bot")]
    bot: PathBuf,

    /// Measured calls per case.
    #[arg(long, default_value_t = 2_000)]
    iterations: usize,

    /// Unmeasured calls first, so the numbers exclude connection warm-up,
    /// lazy allocation and the first-call HTTP/2 settings exchange.
    #[arg(long, default_value_t = 200)]
    warmup: usize,

    /// `tcp`, `unix`, or `both`.
    #[arg(long, default_value = "both")]
    transport: String,

    #[arg(long)]
    socket_dir: Option<PathBuf>,

    /// Emit one JSON object per row instead of a table.
    #[arg(long)]
    json: bool,
}

#[tokio::main]
async fn main() -> Result<()> {
    // The observer effect, handled before it can be measured. M4 gave every
    // handler an INFO event; at 2000 iterations that is 2000 formatted lines
    // per case, written to an inherited stderr, inside the timing loop. The
    // first version of this harness measured `tracing` as much as it measured
    // the transport. Bots inherit this variable when the engine spawns them.
    if std::env::var_os("RUST_LOG").is_none() {
        std::env::set_var("RUST_LOG", "warn");
    }

    let telemetry = ni_telemetry::init("ni-bench");
    let cli = Cli::parse();

    let transports: Vec<Transport> = match cli.transport.as_str() {
        "both" => vec![Transport::Tcp, Transport::Unix],
        other => vec![other
            .parse()
            .map_err(|error: String| anyhow::anyhow!(error))?],
    };

    let result = run(&cli, &transports).await;
    telemetry.shutdown();

    let rows = result?;

    if cli.json {
        for row in &rows {
            println!("{}", serde_json::to_string(row)?);
        }
    } else {
        print_table(&rows);
        print_comparison(&rows);
    }

    Ok(())
}

async fn run(cli: &Cli, transports: &[Transport]) -> Result<Vec<Summary>> {
    // Built once, outside the timing loops, and reused for every transport.
    // Two transports measuring two different payloads would measure nothing.
    let small_replay = replay_of_at_least(1_024);
    let small_mid_replay = replay_of_at_least(1024 * 10);
    let mid_replay = replay_of_at_least(1024 * 128);
    let large_replay = replay_of_at_least(1_024 * 1_024);

    let mut rows = Vec::new();

    for transport in transports {
        let sockets = match transport {
            Transport::Tcp => None,
            Transport::Unix => {
                let base = cli
                    .socket_dir
                    .clone()
                    .unwrap_or_else(SocketDir::default_base);
                Some(SocketDir::create(&base, "bench")?)
            }
        };

        let listen = match &sockets {
            Some(dir) => Listen::Unix(dir.socket("bench")?),
            None => Listen::Tcp,
        };

        let mut bot = BotProcess::spawn(&cli.bot, "bench bot", &listen)
            .await
            .with_context(|| {
                format!(
                    "could not start {} over {}",
                    cli.bot.display(),
                    transport.as_str()
                )
            })?;

        eprintln!(
            "measuring {} over {} ({} iterations, {} warmup)",
            bot.endpoint(),
            transport.as_str(),
            cli.iterations,
            cli.warmup
        );

        rows.push(measure_identify(&mut bot, *transport, cli).await?);
        rows.push(measure_get_orders(&mut bot, *transport, cli).await?);
        rows.push(
            measure_replay(
                &mut bot,
                *transport,
                cli,
                &small_replay,
                "SubmitReplay 1KiB",
            )
            .await?,
        );
        rows.push(
            measure_replay(
                &mut bot,
                *transport,
                cli,
                &small_mid_replay,
                "SubmitReplay 10KiB",
            )
            .await?,
        );
        rows.push(
            measure_replay(
                &mut bot,
                *transport,
                cli,
                &mid_replay,
                "SubmitReplay 128KiB",
            )
            .await?,
        );
        rows.push(
            measure_replay(
                &mut bot,
                *transport,
                cli,
                &large_replay,
                "SubmitReplay 1MiB",
            )
            .await?,
        );

        bot.shutdown().await?;
    }

    Ok(rows)
}

async fn measure_identify(
    bot: &mut BotProcess,
    transport: Transport,
    cli: &Cli,
) -> Result<Summary> {
    let message = IdentifyRequest {
        protocol_version: PROTOCOL_VERSION,
    };
    let bytes = message.encoded_len();

    let mut samples = Samples::with_capacity(cli.iterations);

    for iteration in 0..cli.warmup + cli.iterations {
        let mut request = Request::new(message);
        request.set_timeout(CALL_DEADLINE);

        let started = Instant::now();
        bot.client.identify(request).await?;
        let latency = started.elapsed();

        if iteration >= cli.warmup {
            samples.push(latency);
        }
    }

    Ok(samples.summarise(transport.as_str(), "Identify", bytes))
}

async fn measure_get_orders(
    bot: &mut BotProcess,
    transport: Transport,
    cli: &Cli,
) -> Result<Summary> {
    // The bot answers SHRUBBERY_REQUIRED for a match it has never heard of,
    // so the benchmark has to play by the contract's rules like anyone else.
    let state = ni_game::standard_match(ni_game::Rules::standard());
    let match_id = "ni-bench";

    bot.client
        .new_match(Request::new(new_match_request(
            &state,
            match_id,
            ni_game::Chapter::A,
            TimeControl::standard(),
        )))
        .await?;

    let message = GetOrdersRequest {
        match_id: match_id.to_string(),
        turn: state.turn,
        view: Some(battlefield_view(&state)),
    };
    let bytes = message.encoded_len();

    let mut samples = Samples::with_capacity(cli.iterations);

    for iteration in 0..cli.warmup + cli.iterations {
        let mut request = Request::new(message.clone());
        request.set_timeout(CALL_DEADLINE);

        let started = Instant::now();
        bot.client.get_orders(request).await?;
        let latency = started.elapsed();

        if iteration >= cli.warmup {
            samples.push(latency);
        }
    }

    Ok(samples.summarise(transport.as_str(), "GetOrders", bytes))
}

async fn measure_replay(
    bot: &mut BotProcess,
    transport: Transport,
    cli: &Cli,
    replay: &Replay,
    case: &str,
) -> Result<Summary> {
    let message = SubmitReplayRequest {
        replay: Some(replay.clone()),
    };
    let bytes = message.encoded_len();

    // A 1 MiB payload at 2000 iterations is 2 GiB of copying for a number
    // that is already stable after a few hundred calls.
    let iterations = if bytes > 512 * 1_024 {
        cli.iterations.min(200)
    } else {
        cli.iterations
    };
    let warmup = cli.warmup.min(iterations / 4);

    let mut samples = Samples::with_capacity(iterations);

    for iteration in 0..warmup + iterations {
        let mut request = Request::new(message.clone());
        request.set_timeout(CALL_DEADLINE);

        let started = Instant::now();
        bot.client.submit_replay(request).await?;
        let latency = started.elapsed();

        if iteration >= warmup {
            samples.push(latency);
        }
    }

    Ok(samples.summarise(transport.as_str(), case, bytes))
}

fn print_table(rows: &[Summary]) {
    println!();
    println!(
        "{:<6} {:<20} {:>9} {:>6} {:>8} {:>8} {:>8} {:>8} {:>8} {:>9}",
        "trans", "case", "bytes", "n", "p50 us", "p90 us", "p99 us", "max us", ">10ms", "calls/s"
    );
    println!("{}", "-".repeat(101));

    for row in rows {
        println!(
            "{:<6} {:<20} {:>9} {:>6} {:>8} {:>8} {:>8} {:>8} {:>8} {:>9.0}",
            row.transport,
            row.case,
            row.payload_bytes,
            row.calls,
            row.p50_us,
            row.p90_us,
            row.p99_us,
            row.max_us,
            row.over_10ms,
            row.calls_per_second(),
        );
    }
}

/// The only number post 6 actually argues about: how much of the median call
/// the wire was responsible for.
fn print_comparison(rows: &[Summary]) {
    // Pair the rows up *before* printing anything. Only cases measured on both
    // transports can be compared, and a single-transport run therefore has
    // nothing to say here — a header with no rows under it would be worse than
    // no header at all.
    let pairs: Vec<(&Summary, &Summary)> = rows
        .iter()
        .filter(|row| row.transport == "tcp")
        .filter_map(|tcp| {
            rows.iter()
                .find(|row| row.transport == "unix" && row.case == tcp.case)
                .map(|unix| (tcp, unix))
        })
        .collect();

    if pairs.is_empty() {
        return;
    }

    println!();
    println!(
        "{:<20} {:>10} {:>10} {:>10} {:>10}",
        "case", "tcp p50", "unix p50", "delta", "unix MiB/s"
    );
    println!("{}", "-".repeat(64));

    for (tcp, unix) in pairs {
        let delta = if tcp.p50_us == 0 {
            0.0
        } else {
            (unix.p50_us as f64 - tcp.p50_us as f64) / tcp.p50_us as f64 * 100.0
        };

        println!(
            "{:<20} {:>10} {:>10} {:>9.1}% {:>10.1}",
            tcp.case,
            tcp.p50_us,
            unix.p50_us,
            delta,
            unix.payload_mib_per_second(),
        );
    }
}
