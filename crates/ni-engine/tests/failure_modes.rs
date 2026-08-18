//! Real processes, real sockets, real misbehaviour.
//!
//! These tests spawn the built binaries, so run `cargo build --workspace`
//! before `cargo test`. They assert one thing above all: whatever a bot
//! does, the engine returns a result instead of hanging or panicking.

use std::{path::PathBuf, time::Duration};

use ni_engine::{
    policy::TimeControl, run_match, BotProcess, ForfeitReason, MatchConclusion, RunOptions,
};
use ni_game::Chapter;
use serde_json::Value;

fn binary(name: &str) -> PathBuf {
    // The test binary lives in target/<profile>/deps/, so the workspace
    // binaries are one directory up.
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

fn options(deadline_ms: u64, strike_limit: u32) -> RunOptions {
    RunOptions {
        delay: Duration::ZERO,
        quiet: true,
        match_id: "m4-test".to_string(),
        time: TimeControl {
            turn_deadline: Duration::from_millis(deadline_ms),
            strike_limit,
        },
        match_log: None,
    }
}

/// A log path unique to one test, so the suite can run in parallel.
fn log_path(name: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("ni-m4-{name}.jsonl"));
    let _ = std::fs::remove_file(&path);
    path
}

fn read_entries(path: &PathBuf) -> Vec<Value> {
    std::fs::read_to_string(path)
        .expect("the engine wrote a match log")
        .lines()
        .map(|line| serde_json::from_str(line).expect("every line is one JSON object"))
        .collect()
}

/// Play `bot A = ni-bot` against a Roger carrying `flags`, and return how it
/// ended. Panics only if the *engine* failed, which is the point.
async fn play_against_roger(flags: &[&str], options: RunOptions) -> MatchConclusion {
    let mut bot_a = BotProcess::spawn(&binary("ni-bot"), "bot A")
        .await
        .expect("reference bot starts");

    let mut bot_b = BotProcess::spawn_with_args(&binary("roger-the-shrubber"), "bot B", flags)
        .await
        .expect("roger starts");

    let conclusion = run_match(&mut bot_a, &mut bot_b, options).await;

    let _ = bot_a.shutdown().await;
    let _ = bot_b.shutdown().await;

    conclusion.expect("the engine survives a hostile bot")
}

#[tokio::test]
async fn a_permanently_slow_bot_runs_out_of_strikes() {
    let conclusion = play_against_roger(&["--sleep-ms", "400"], options(100, 2)).await;

    assert_eq!(
        conclusion,
        MatchConclusion::Forfeit {
            loser: Chapter::B,
            reason: ForfeitReason::Timeout,
        }
    );
}

#[tokio::test]
async fn one_slow_turn_costs_a_turn_not_the_match() {
    let conclusion = play_against_roger(
        &["--sleep-ms", "400", "--sleep-on-turn", "2"],
        options(100, 3),
    )
    .await;

    assert!(
        matches!(conclusion, MatchConclusion::Decided(_)),
        "a single missed deadline should not end the match: {conclusion:?}"
    );
}

#[tokio::test]
async fn a_crashing_bot_forfeits_the_match_without_taking_the_engine_with_it() {
    let conclusion = play_against_roger(&["--crash-on-turn", "4"], options(500, 3)).await;

    assert_eq!(
        conclusion,
        MatchConclusion::Forfeit {
            loser: Chapter::B,
            reason: ForfeitReason::Crash,
        }
    );
}

#[tokio::test]
async fn a_forgetful_bot_is_restored_with_new_match_and_the_match_continues() {
    let conclusion = play_against_roger(&["--forget-on-turn", "4"], options(500, 3)).await;

    assert!(
        matches!(conclusion, MatchConclusion::Decided(_)),
        "SHRUBBERY_REQUIRED should be recoverable: {conclusion:?}"
    );
}

#[tokio::test]
async fn illegal_orders_are_a_wasted_turn_not_a_forfeit() {
    let conclusion = play_against_roger(&["--illegal-orders"], options(500, 3)).await;

    // Chapter B never legally acts, so chapter A wins on the board.
    assert_eq!(conclusion.winner(), Some(Chapter::A));
    assert!(matches!(conclusion, MatchConclusion::Decided(_)));
}

#[tokio::test]
async fn ordering_someone_elses_knights_is_also_just_illegal() {
    let conclusion = play_against_roger(&["--steal-knights"], options(500, 3)).await;

    assert_eq!(conclusion.winner(), Some(Chapter::A));
}

#[tokio::test]
async fn a_broken_turn_echo_is_a_protocol_forfeit() {
    let conclusion = play_against_roger(&["--wrong-turn"], options(500, 2)).await;

    assert_eq!(
        conclusion,
        MatchConclusion::Forfeit {
            loser: Chapter::B,
            reason: ForfeitReason::Protocol,
        }
    );
}

#[tokio::test]
async fn a_normal_match_writes_a_readable_jsonl_log() {
    let path = log_path("normal");
    let mut options = options(500, 3);
    options.match_log = Some(path.clone());

    let conclusion = play_against_roger(&[], options).await;
    assert!(matches!(conclusion, MatchConclusion::Decided(_)));

    let entries = read_entries(&path);

    assert_eq!(entries.first().unwrap()["kind"], "match_started");
    assert_eq!(entries.last().unwrap()["kind"], "match_ended");

    let turns: Vec<_> = entries
        .iter()
        .filter(|entry| entry["kind"] == "turn")
        .collect();

    assert!(turns.len() > 4, "a full match has many turns");
    assert!(turns.iter().all(|turn| turn["status"] == "ok"));
    assert!(turns.iter().all(|turn| turn["attempts"] == 1));
    assert!(turns
        .iter()
        .all(|turn| turn["latency_us"].as_u64().is_some()));
    assert_eq!(
        entries.last().unwrap()["turns"].as_u64().unwrap() as usize,
        turns.len()
    );
}

#[tokio::test]
async fn a_forfeited_turn_is_visible_in_the_log() {
    let path = log_path("timeout");
    let mut options = options(100, 2);
    options.match_log = Some(path.clone());

    play_against_roger(&["--sleep-ms", "400"], options).await;

    let entries = read_entries(&path);
    let timeouts: Vec<_> = entries
        .iter()
        .filter(|entry| entry["status"] == "timeout")
        .collect();

    assert_eq!(timeouts.len(), 2, "two strikes, two log lines");
    assert!(timeouts
        .iter()
        .all(|turn| turn["deadline_exceeded"] == true));
    assert!(timeouts.iter().all(|turn| turn["acting"] == "B"));
    assert!(timeouts[0]["detail"]
        .as_str()
        .unwrap()
        .contains("no answer within 100ms"));
}

#[tokio::test]
async fn a_recovered_turn_records_two_attempts() {
    let path = log_path("recovery");
    let mut options = options(500, 3);
    options.match_log = Some(path.clone());

    play_against_roger(&["--forget-on-turn", "4"], options).await;

    let entries = read_entries(&path);
    let recovered: Vec<_> = entries
        .iter()
        .filter(|entry| entry["attempts"] == 2)
        .collect();

    assert_eq!(recovered.len(), 1, "exactly one turn needed a shrubbery");
    assert_eq!(recovered[0]["turn"], 4);
    assert_eq!(recovered[0]["status"], "ok");
}
