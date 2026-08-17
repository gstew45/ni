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
        match_id: "m3-test".to_string(),
        time: TimeControl {
            turn_deadline: Duration::from_millis(deadline_ms),
            strike_limit,
        },
    }
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
