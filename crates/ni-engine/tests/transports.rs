//! Transport parity: the same match, the same failures, two socket families.
//!
//! These tests spawn the built binaries, so run `cargo build --workspace`
//! before `cargo test`. Their job is to make M5's central claim falsifiable —
//! if swapping the transport changed a *result*, one of these would go red.

use std::{path::PathBuf, time::Duration};

use ni_engine::{
    policy::TimeControl, run_match, transport::dial, BotProcess, ForfeitReason, Listen,
    MatchConclusion, RunOptions, SocketDir, Transport,
};
use ni_game::Chapter;
use serde_json::Value;

fn binary(name: &str) -> PathBuf {
    let mut path = std::env::current_exe().expect("test binary has a path");
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }

    let path = path.join(name);
    assert!(
        path.exists(),
        "{} is missing — run `cargo build --workspace` first",
        path.display()
    );
    path
}

fn sockets(name: &str) -> SocketDir {
    SocketDir::create(&std::env::temp_dir().join("ni-m5-transports"), name)
        .expect("socket directory is created")
}

fn options(match_id: &str, transport: Transport, deadline_ms: u64, strikes: u32) -> RunOptions {
    RunOptions {
        delay: Duration::ZERO,
        quiet: true,
        match_id: match_id.to_string(),
        time: TimeControl {
            turn_deadline: Duration::from_millis(deadline_ms),
            strike_limit: strikes,
        },
        match_log: None,
        transport,
    }
}

/// Play `ni-bot` against a Roger carrying `flags`, on whichever transport
/// `options` names. Returns the conclusion and the socket directory, so a
/// caller can inspect the filesystem before it is cleaned up.
async fn play(flags: &[&str], options: RunOptions) -> (MatchConclusion, Option<SocketDir>) {
    let dir = match options.transport {
        Transport::Tcp => None,
        Transport::Unix => Some(sockets(&options.match_id)),
    };

    let listen = |label: &str| match &dir {
        Some(dir) => Listen::Unix(dir.socket(label).expect("socket path fits")),
        None => Listen::Tcp,
    };

    let mut bot_a = BotProcess::spawn(&binary("ni-bot"), "bot A", &listen("a"))
        .await
        .expect("reference bot starts");

    let mut bot_b =
        BotProcess::spawn_with_args(&binary("roger-the-shrubber"), "bot B", flags, &listen("b"))
            .await
            .expect("roger starts");

    let conclusion = run_match(&mut bot_a, &mut bot_b, options).await;

    let _ = bot_a.shutdown().await;
    let _ = bot_b.shutdown().await;

    (
        conclusion.expect("the engine survives whichever transport it was given"),
        dir,
    )
}

/// The milestone, as one assertion.
///
/// Same rules engine, same bots, same deadlines, different socket family —
/// and a deterministic game has no excuse for a different answer.
#[tokio::test]
async fn a_unix_socket_match_ends_exactly_as_the_loopback_match_did() {
    let (over_tcp, _) = play(&[], options("parity-tcp", Transport::Tcp, 500, 3)).await;
    let (over_unix, _) = play(&[], options("parity-unix", Transport::Unix, 500, 3)).await;

    assert!(matches!(over_tcp, MatchConclusion::Decided(_)));
    assert_eq!(
        over_tcp, over_unix,
        "the transport changed the outcome of a deterministic match"
    );
}

/// Post 6's most quotable finding, as a test: the ambiguity of a missed
/// deadline is a property of the *boundary*, not of the network.
#[tokio::test]
async fn the_ambiguous_case_survives_the_transport_swap() {
    let (conclusion, _) = play(
        &["--sleep-ms", "400"],
        options("slow-unix", Transport::Unix, 100, 2),
    )
    .await;

    assert_eq!(
        conclusion,
        MatchConclusion::Forfeit {
            loser: Chapter::B,
            reason: ForfeitReason::Timeout,
        },
        "a deadline over a Unix socket is still a deadline"
    );
}

#[tokio::test]
async fn a_bot_that_dies_on_a_unix_socket_is_still_merely_unreachable() {
    let (conclusion, _) = play(
        &["--crash-on-turn", "4"],
        options("crash-unix", Transport::Unix, 500, 3),
    )
    .await;

    assert_eq!(
        conclusion,
        MatchConclusion::Forfeit {
            loser: Chapter::B,
            reason: ForfeitReason::Crash,
        }
    );
}

#[tokio::test]
async fn the_match_log_records_which_transport_carried_the_match() {
    let path = std::env::temp_dir().join("ni-m5-transport-log.jsonl");
    let _ = std::fs::remove_file(&path);

    let mut options = options("logged-unix", Transport::Unix, 500, 3);
    options.match_log = Some(path.clone());

    let (_, _dir) = play(&[], options).await;

    let started: Value = serde_json::from_str(
        std::fs::read_to_string(&path)
            .expect("the engine wrote a match log")
            .lines()
            .next()
            .expect("the log has a first line"),
    )
    .expect("the first line is JSON");

    assert_eq!(started["kind"], "match_started");
    assert_eq!(started["transport"], "unix");
}

/// A port disappears when its socket closes. A path does not, so somebody has
/// to delete it — and this is the test that says who.
#[tokio::test]
async fn every_socket_file_is_gone_once_the_match_is_over() {
    let (_, dir) = play(&[], options("cleanup-unix", Transport::Unix, 500, 3)).await;
    let dir = dir.expect("a unix match has a socket directory");
    let path = dir.path().to_path_buf();

    // The bots unlink their own sockets on SIGTERM.
    assert!(
        std::fs::read_dir(&path)
            .expect("the directory still exists")
            .next()
            .is_none(),
        "a bot left its socket file behind: {}",
        path.display()
    );

    // And the engine owns the directory itself.
    drop(dir);
    assert!(!path.exists(), "the socket directory outlived the match");
}

/// The one place the transports genuinely differ in *shape*: what "nobody is
/// there" looks like before a single gRPC frame has been written.
#[tokio::test]
async fn dialling_a_path_with_no_listener_fails_instead_of_hanging() {
    let path = std::env::temp_dir().join("ni-m5-nobody-here.sock");
    let _ = std::fs::remove_file(&path);

    let error = dial(
        &format!("unix://{}", path.display()),
        Duration::from_millis(200),
    )
    .await
    .expect_err("there is no listener");

    // ENOENT, not ECONNREFUSED: a missing path is not a closed port.
    let text = format!("{error:#}");
    assert!(
        text.contains("could not connect to unix://"),
        "unexpected error: {text}"
    );
}
