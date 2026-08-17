# M3 step by step — deadlines, strikes, and bots that misbehave

This is the hands-on workbook for milestone **M3**, in the same shape as
[`m2-step-by-step.md`](m2-step-by-step.md): which file to open, what to put
in it, what the Rust means, why the design is the way it is, and what command
to run before continuing.

M2 got a match played over gRPC. It assumed both bots behave. M3 removes that
assumption, and does it in the way blog posts 3 and 4 need: a per-turn
*deadline* that travels on the wire, a defined answer for every way a bot can
fail, and a hostile bot that produces those failures on demand.

Every code block below was compiled, formatted, clippy-cleaned and run against
the repository's M2 implementation with tonic/prost 0.13. Every terminal
transcript is real output, not a sketch.

## How to use this workbook

Work through the checkpoints in order. At each checkpoint:

1. edit only the files named in that checkpoint;
2. run `cargo fmt`;
3. run the checkpoint command;
4. fix errors before moving on;
5. make a short note about what you observed.

Run every command from the repository root:

```sh
cd /home/gstewart/projects/ni
```

## What M2 leaves broken

Read this list before writing code; each line is a bug you are about to fix.

- **The engine can hang forever.** `bot.client.get_orders(request).await` has
  no deadline. A bot that sleeps stops the match with no output and no exit.
- **A crashed bot is an engine error.** `run_match` uses `?`, so a transport
  failure bubbles up as an `anyhow::Error`. The match has no *result*, which
  means a tournament could not record one.
- **The turn echo aborts the match.** M2 checks the echoed turn with
  `ensure!`, so one confused response kills a match that could have continued.
- **`SHRUBBERY_REQUIRED` is defined but never handled.** The bot raises it,
  the proto documents it, and the engine treats it like any other error.
- **Nothing in the workspace can misbehave.** Every failure path is untested
  because there is nothing to test it with.

## What M3 adds

- A **per-turn deadline** set with `Request::set_timeout`, which puts
  `grpc-timeout` on the wire — the bot is *told* its budget rather than being
  cut off by a stopwatch it cannot see.
- **Turn forfeits and strikes**: a missed deadline costs the turn; a
  configurable number of *consecutive* forfeited turns costs the match.
- **`SHRUBBERY_REQUIRED` recovery**: re-send `NewMatch` at the current turn,
  then retry the same `GetOrders` — safe because the turn number is echoed.
- **A conclusion type that can express a forfeit**, so every match ends with a
  result rather than an error.
- **`roger-the-shrubber`**, the deliberately hostile bot: sleeps, crashes,
  forgets matches, orders knights he does not own, echoes the wrong turn.
- **Integration tests** that spawn real processes and assert the engine
  survives each of those.

## What M3 deliberately leaves alone

- `tracing` spans, W3C trace context and the JSONL match log — that is M4.
- `SubmitReplay` stays a stub.
- Unix domain sockets stay unwritten; M3 is still loopback TCP (M5).
- The rules in `ni-game` do not change at all. Not one line.

## Design decisions this milestone settles

| Question | M3's answer | Why |
|---|---|---|
| Timeout loses the turn or the match? | The turn, with a strike counter (default 3) for the match | Keeps games alive, and a strike limit still stops a dead bot from playing 100 empty turns |
| Is `GetOrders` retried? | Only after `SHRUBBERY_REQUIRED`, and only once | The turn echo makes a retry safe; retrying a *timeout* is the ambiguous case post 4 is about, so the engine deliberately does not |
| Where do forfeits live? | In `ni-engine`, never in `ni-game` | Forfeits are judgements about a *process*, not about the rules. `ni-game` stays pure |
| What does a forfeit do to the exit code? | Nothing — the engine exits 0 | A forfeit is a match result. A non-zero exit would mean *the engine* failed |
| Who owns the clock? | The engine, in `TimeControl` | The bot is told the numbers in `NewMatch`, but nothing depends on it honouring them |

## What you will create

```text
crates/ni-engine/src/
  policy.rs              NEW  time control, failure taxonomy, strikes, conclusions
  match_runner.rs        rewritten  deadlines, forfeits, recovery
  process.rs             edited     startup deadline, extra args, exit status
  convert.rs             edited     time control on the wire, resume request
  render.rs              edited     forfeited turns and forfeited matches
  lib.rs / main.rs       edited     module list, CLI flags
crates/ni-engine/tests/
  failure_modes.rs       NEW  integration tests with real processes

crates/ni-bot/src/
  server.rs              NEW  listener + readiness line, shared by all bots
  lib.rs / main.rs       edited

crates/roger-the-shrubber/     NEW CRATE
  Cargo.toml
  src/lib.rs             the hostile service
  src/main.rs            flags and startup
```

Plus the workspace `Cargo.toml`.

## A small Rust map for M3

M2's map still applies. These are the new shapes.

```rust
enum TurnOutcome {
    Orders(Vec<Order>),
    TurnForfeited { reason: ForfeitReason, detail: String },
}
```

An enum variant can carry data — a tuple, or named fields like a struct. This
is how a function returns "one of several *different* answers" without an
`Option` of a tuple of a bool. `match` then forces you to handle each.

```rust
let answer = tokio::time::timeout(Duration::from_secs(5), some_future).await;
```

`timeout` wraps any future in a clock. The result is
`Result<T, Elapsed>` where `T` is whatever the inner future returned — so if
the inner future itself returns a `Result`, you get a `Result<Result<..>>` and
two `?` in a row. That double-unwrap is not a mistake; it is "did it finish?"
followed by "did it succeed?".

```rust
let mut request = Request::new(message);
request.set_timeout(Duration::from_millis(500));
```

`tonic::Request` is the message plus its call metadata. `set_timeout` is the
only line in this milestone that puts a deadline on the wire.

```rust
let started = Instant::now();
// ...
let elapsed = started.elapsed();
```

A monotonic stopwatch. Unlike wall-clock time it cannot go backwards, which
is what you want for measuring a call.

```rust
if self.already_forgot.swap(true, Ordering::SeqCst) { return; }
```

`AtomicBool::swap` sets the value and returns the old one, in one indivisible
step. Roger uses it as a latch: the first caller sees `false` and acts, every
later caller sees `true` and does not. It needs no `&mut self`, which matters
because a gRPC handler only ever gets `&self`.

```rust
assert!(matches!(value, SomeEnum::Variant(_)));
```

`matches!` is `match` compressed into a `bool`: true when the value has that
shape. Useful when you care about the variant but not its payload.

```rust
let (bot, strikes) = match acting {
    Chapter::A => (&mut *bot_a, &mut strikes_a),
    Chapter::B => (&mut *bot_b, &mut strikes_b),
};
```

`&mut *bot_a` is a *reborrow*: `bot_a` is already an `&mut BotProcess`, and
this hands out a shorter-lived borrow of the same thing instead of moving it.
Without the `*`, the first loop iteration would consume `bot_a` and the second
would not compile.

```rust
#[derive(clap::Args)]
struct Mischief { /* ... */ }

#[derive(Parser)]
struct Cli {
    #[command(flatten)]
    mischief: Mischief,
}
```

`Args` is "a group of flags"; `Parser` is "a whole command line". `flatten`
splices the group into the command, so `Mischief`'s fields become top-level
flags while staying one value you can pass around.


---

## Checkpoint 0 — prove M2 is healthy

```sh
cargo test --workspace
cargo build --workspace
./target/debug/ni-engine run \
  --bot-a target/debug/ni-bot \
  --bot-b target/debug/ni-bot
```

Expected: tests pass and the match ends with `result: chapter A wins by
elimination`.

Now break it on purpose, so you have felt the thing you are fixing. There is
no hostile bot yet, so use the shell:

```sh
./target/debug/ni-engine run --bot-a target/debug/ni-bot --bot-b /bin/sleep
```

The engine prints nothing and never exits. `Ctrl-C` it. That hang is M3's
reason to exist.


---

## Checkpoint 1 — the decisions, with no I/O

Everything M3 adds is really one question — *what does this failure mean?* —
and the answer should not be tangled up with sockets. So it goes in its own
module, where it can be tested without starting a process.

### 1.1 Replace `crates/ni-engine/src/lib.rs`

```rust
//! M3 match orchestration around the pure `ni-game` rules engine:
//! deadlines, strikes, recovery and forfeits on top of M2's core loop.

pub mod convert;
pub mod match_runner;
pub mod policy;
pub mod process;
pub mod render;

pub use match_runner::{run_match, RunOptions};
pub use policy::{ForfeitReason, MatchConclusion, TimeControl};
pub use process::BotProcess;
```

Declaring `pub mod policy;` before the file exists is fine — you are about to
create it, and nothing compiles until you do.

### 1.2 Create `crates/ni-engine/src/policy.rs`

Start with the clock.

```rust
//! M3 time control and failure policy.
//!
//! Everything in this module is a *decision*, not an action: how long a bot
//! may think, what a gRPC status means, how many bad turns a bot is allowed
//! before it loses the match. No async, no gRPC calls, no I/O — which means
//! all of it is unit-testable without starting a process.

use std::time::Duration;

use ni_game::{Chapter, EndReason, MatchStatus};
use ni_proto::{
    ni::v1::{MatchEndReason, MatchOutcome},
    SHRUBBERY_REQUIRED,
};
use tonic::{Code, Status};

/// The engine's clock policy. These two numbers are engine concerns, which
/// is why they are not in `ni_game::Rules`: the rules crate has no idea what
/// a deadline is.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimeControl {
    /// How long a bot may take to answer one `GetOrders`.
    pub turn_deadline: Duration,
    /// Consecutive forfeited turns before the match itself is forfeited.
    pub strike_limit: u32,
}

impl TimeControl {
    pub fn standard() -> Self {
        Self {
            turn_deadline: Duration::from_millis(500),
            strike_limit: 3,
        }
    }

    /// The wire form: `ni.v1.Rules.turn_deadline_ms` is a `uint32`.
    pub fn turn_deadline_ms(self) -> u32 {
        u32::try_from(self.turn_deadline.as_millis()).unwrap_or(u32::MAX)
    }
}

impl Default for TimeControl {
    fn default() -> Self {
        Self::standard()
    }
}
```

**Why `TimeControl` is not in `ni_game::Rules`.** The rules crate is pure and
knows nothing about time; a deadline is a property of *calling another
process*, not of the game. `ni.v1.Rules` does carry `turn_deadline_ms` and
`timeout_strike_limit`, because the bot is entitled to know its budget — but
the fields are filled in by the engine at the wire boundary, in `convert.rs`,
and never round-trip into `ni-game`.

`turn_deadline_ms` exists because `Duration` counts nanoseconds in a `u128`
and the proto field is a `uint32` of milliseconds. `u32::try_from(...)
.unwrap_or(u32::MAX)` converts without ever panicking: an absurd deadline
saturates instead of crashing the engine.

Next, how a match can end.

```rust
/// Why the engine stopped trusting a bot.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForfeitReason {
    /// Ran out of consecutive turn deadlines.
    Timeout,
    /// Unreachable: crashed, exited, or dropped the connection.
    Crash,
    /// Answered, but broke the contract.
    Protocol,
}

/// How a match ended. `ni_game::MatchStatus` covers the outcomes the *rules*
/// can produce; forfeits are engine judgements, so they live out here.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MatchConclusion {
    Decided(MatchStatus),
    Forfeit {
        loser: Chapter,
        reason: ForfeitReason,
    },
}

impl MatchConclusion {
    pub fn winner(self) -> Option<Chapter> {
        match self {
            MatchConclusion::Decided(MatchStatus::Winner { chapter, .. }) => Some(chapter),
            MatchConclusion::Decided(_) => None,
            MatchConclusion::Forfeit { loser, .. } => Some(loser.opponent()),
        }
    }

    /// `ni.v1.MatchOutcome` from the receiving bot's point of view.
    pub fn outcome_for(self, recipient: Chapter) -> i32 {
        let outcome = match self {
            MatchConclusion::Decided(MatchStatus::InProgress) => MatchOutcome::Unspecified,
            MatchConclusion::Decided(MatchStatus::Draw { .. }) => MatchOutcome::Draw,
            _ => match self.winner() {
                Some(winner) if winner == recipient => MatchOutcome::Win,
                Some(_) => MatchOutcome::Loss,
                None => MatchOutcome::Unspecified,
            },
        };

        outcome as i32
    }

    pub fn end_reason(self) -> i32 {
        let reason = match self {
            MatchConclusion::Decided(MatchStatus::InProgress) => MatchEndReason::Unspecified,
            MatchConclusion::Decided(
                MatchStatus::Winner {
                    reason: EndReason::Elimination,
                    ..
                }
                | MatchStatus::Draw {
                    reason: EndReason::Elimination,
                },
            ) => MatchEndReason::Elimination,
            MatchConclusion::Decided(
                MatchStatus::Winner {
                    reason: EndReason::TurnCap,
                    ..
                }
                | MatchStatus::Draw {
                    reason: EndReason::TurnCap,
                },
            ) => MatchEndReason::TurnCap,
            MatchConclusion::Forfeit {
                reason: ForfeitReason::Timeout,
                ..
            } => MatchEndReason::ForfeitTimeout,
            MatchConclusion::Forfeit {
                reason: ForfeitReason::Crash,
                ..
            } => MatchEndReason::ForfeitCrash,
            MatchConclusion::Forfeit {
                reason: ForfeitReason::Protocol,
                ..
            } => MatchEndReason::ForfeitProtocol,
        };

        reason as i32
    }
}
```

**Why a new type instead of extending `ni_game::MatchStatus`.** A forfeit is
not a fact about the board. `ni-game` cannot know that a process stopped
answering, and if you taught it, every rules test would suddenly need an
opinion about gRPC. `MatchConclusion` wraps the rules' verdict
(`Decided`) and adds the engine's own (`Forfeit`), which keeps the pure crate
pure and puts the mapping to `ni.v1.MatchOutcome` / `ni.v1.MatchEndReason` in
exactly one place.

Note `outcome_for`: it answers *from the receiving bot's point of view*, so
the same conclusion sends `WIN` to one bot and `LOSS` to the other. The
`|` in the `end_reason` patterns is an or-pattern: `Winner { reason:
Elimination, .. } | Draw { reason: Elimination }` matches either shape and
binds nothing.

Now the taxonomy — the heart of the milestone.

```rust
/// What a failed call *means*, stripped of gRPC vocabulary.
///
/// This is the whole of the engine's error taxonomy. Everything downstream
/// switches on these four cases, never on a raw [`Code`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CallFailure {
    /// The bot did not answer inside the deadline.
    Timeout,
    /// The bot has never heard of this match: re-send `NewMatch`, then retry.
    NeedsShrubbery,
    /// Nobody is home. Retrying will not help.
    Unreachable(String),
    /// The bot answered with something the contract does not allow.
    Protocol(String),
}

pub fn classify(status: &Status) -> CallFailure {
    match status.code() {
        // DEADLINE_EXCEEDED is the documented answer. CANCELLED shows up
        // when the local side gives up first, which is the same event seen
        // from the other end of the stream.
        Code::DeadlineExceeded | Code::Cancelled => CallFailure::Timeout,
        Code::FailedPrecondition if status.message().contains(SHRUBBERY_REQUIRED) => {
            CallFailure::NeedsShrubbery
        }
        // UNAVAILABLE is the transport's "connection refused / broken pipe".
        // UNKNOWN is what tonic reports for an h2 stream that died mid-call,
        // which is exactly what a bot process exiting looks like.
        Code::Unavailable | Code::Unknown => CallFailure::Unreachable(describe(status)),
        _ => CallFailure::Protocol(describe(status)),
    }
}

pub fn describe(status: &Status) -> String {
    format!("{:?}: {}", status.code(), status.message())
}
```

**Four cases, deliberately.** gRPC has seventeen status codes; the engine has
four *reactions*: wait for next turn, re-send `NewMatch`, give up on the
process, or blame the bot's answer. Collapsing codes into meanings here means
no other part of the engine ever writes `Code::` again — and when you later
discover a code you mis-filed, there is one function to fix and one test to
update.

`Code::Cancelled` sitting next to `Code::DeadlineExceeded` is not defensive
padding. It is what tonic 0.13 actually returns to the client when a
`set_timeout` deadline elapses; Checkpoint 10 shows the transcript. Had this
`match` listed only `DeadlineExceeded`, every timeout in the milestone would
have been misclassified as a broken contract, and the strike counter would
still have "worked" — with the wrong reason attached. That is the kind of bug
only an observed status code catches.

Finally, the strike counter.

```rust
/// Consecutive forfeited turns for one chapter.
///
/// "Consecutive" is the important word: one answered turn wipes the slate,
/// so a bot that misses a deadline occasionally keeps playing, and only a
/// bot that has stopped answering loses the match.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Strikes {
    count: u32,
    last: Option<ForfeitReason>,
}

impl Strikes {
    pub fn record(&mut self, reason: ForfeitReason) {
        self.count += 1;
        self.last = Some(reason);
    }

    pub fn clear(&mut self) {
        self.count = 0;
        self.last = None;
    }

    pub fn count(self) -> u32 {
        self.count
    }

    /// `Some(reason)` once the limit is reached — the match is over and this
    /// is why.
    pub fn exhausted(self, limit: u32) -> Option<ForfeitReason> {
        match self.last {
            Some(reason) if self.count >= limit => Some(reason),
            _ => None,
        }
    }
}
```

`count` and `last` are private, so the only way to move the counter is
`record` / `clear`. `exhausted` returns `Option<ForfeitReason>` rather than a
`bool` because the caller needs both facts at once — *is it over* and *why* —
and an `Option` makes it impossible to have one without the other.

### 1.3 Append the tests to `policy.rs`

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadline_and_cancellation_are_the_same_event() {
        assert_eq!(
            classify(&Status::deadline_exceeded("late")),
            CallFailure::Timeout
        );
        assert_eq!(
            classify(&Status::cancelled("Timeout expired")),
            CallFailure::Timeout
        );
    }

    #[test]
    fn only_the_shrubbery_message_means_re_send_new_match() {
        assert_eq!(
            classify(&Status::failed_precondition(SHRUBBERY_REQUIRED)),
            CallFailure::NeedsShrubbery
        );

        assert!(matches!(
            classify(&Status::failed_precondition("something else")),
            CallFailure::Protocol(_)
        ));
    }

    #[test]
    fn transport_failures_are_separated_from_contract_failures() {
        assert!(matches!(
            classify(&Status::unavailable("connection refused")),
            CallFailure::Unreachable(_)
        ));
        assert!(matches!(
            classify(&Status::unknown("transport error")),
            CallFailure::Unreachable(_)
        ));
        assert!(matches!(
            classify(&Status::invalid_argument("view is required")),
            CallFailure::Protocol(_)
        ));
    }

    #[test]
    fn one_good_turn_clears_the_strikes() {
        let mut strikes = Strikes::default();

        strikes.record(ForfeitReason::Timeout);
        strikes.record(ForfeitReason::Timeout);
        assert_eq!(strikes.count(), 2);
        assert_eq!(strikes.exhausted(3), None);

        strikes.clear();
        strikes.record(ForfeitReason::Timeout);
        assert_eq!(strikes.exhausted(3), None);

        strikes.record(ForfeitReason::Timeout);
        strikes.record(ForfeitReason::Protocol);
        assert_eq!(strikes.exhausted(3), Some(ForfeitReason::Protocol));
    }

    #[test]
    fn a_forfeit_is_a_win_for_the_other_chapter() {
        let conclusion = MatchConclusion::Forfeit {
            loser: Chapter::B,
            reason: ForfeitReason::Timeout,
        };

        assert_eq!(conclusion.winner(), Some(Chapter::A));
        assert_eq!(conclusion.outcome_for(Chapter::A), MatchOutcome::Win as i32);
        assert_eq!(
            conclusion.outcome_for(Chapter::B),
            MatchOutcome::Loss as i32
        );
        assert_eq!(
            conclusion.end_reason(),
            MatchEndReason::ForfeitTimeout as i32
        );
    }

    #[test]
    fn decided_matches_still_map_to_the_wire_enums() {
        let conclusion = MatchConclusion::Decided(MatchStatus::Winner {
            chapter: Chapter::A,
            reason: EndReason::Elimination,
        });

        assert_eq!(conclusion.outcome_for(Chapter::A), MatchOutcome::Win as i32);
        assert_eq!(conclusion.end_reason(), MatchEndReason::Elimination as i32);

        let draw = MatchConclusion::Decided(MatchStatus::Draw {
            reason: EndReason::TurnCap,
        });

        assert_eq!(draw.winner(), None);
        assert_eq!(draw.outcome_for(Chapter::B), MatchOutcome::Draw as i32);
        assert_eq!(draw.end_reason(), MatchEndReason::TurnCap as i32);
    }
}
```

These tests are worth more than they look. `Status::deadline_exceeded("late")`
constructs a status locally — no server, no socket, no async — so the entire
failure policy is testable in microseconds. The last two tests pin the wire
mapping, which is the part that will break silently if someone reorders the
proto enums.

### 1.4 Run the checkpoint

```sh
cargo fmt
cargo test -p ni-engine --lib policy
```

Expected: 6 passed. `--lib` compiles only the library, which keeps the cycle
fast; the CLI catches up in Checkpoint 6. Nothing else in the engine knows
this module exists yet, which is the point — the decisions were worth writing
down before anything depended on them.


---

## Checkpoint 2 — put the clock on the wire

Two different things travel now: the deadline as *configuration* (in
`NewMatch`, so the bot can budget) and the deadline as *enforcement* (as a
gRPC deadline on each call). Confusing the two is the classic mistake — a
number in a config message enforces nothing.

### 2.1 Edit `crates/ni-engine/src/convert.rs`

Add the import:

```rust
use crate::policy::TimeControl;
```

Replace `rules_to_proto`, which hardcoded two zeros in M2:

```rust
pub fn rules_to_proto(state: &MatchState, time: TimeControl) -> ProtoRules {
    ProtoRules {
        knight_hp: state.rules.knight_hp,
        move_range: state.rules.move_range,
        attack_range: state.rules.attack_range,
        attack_damage: state.rules.attack_damage,
        cover_damage_reduction: state.rules.cover_damage_reduction,
        turn_cap: state.rules.turn_cap,
        turn_deadline_ms: time.turn_deadline_ms(),
        timeout_strike_limit: time.strike_limit,
    }
}
```

Then replace `new_match_request` and add its mid-match twin:

```rust
/// A fresh match. `turn: 0` is the contract's way of saying "start here".
pub fn new_match_request(
    state: &MatchState,
    match_id: &str,
    chapter: GameChapter,
    time: TimeControl,
) -> NewMatchRequest {
    NewMatchRequest {
        match_id: match_id.to_string(),
        chapter: chapter_to_proto(chapter),
        board: Some(board_layout(state)),
        rules: Some(rules_to_proto(state, time)),
        turn: 0,
    }
}

/// The same message re-sent mid-match after `SHRUBBERY_REQUIRED`, carrying
/// the turn the match resumes at. The position itself rides in the next
/// `GetOrders` view — the engine holds the truth, so recovery is a re-send,
/// not a negotiation.
pub fn resume_match_request(
    state: &MatchState,
    match_id: &str,
    chapter: GameChapter,
    time: TimeControl,
) -> NewMatchRequest {
    NewMatchRequest {
        turn: state.turn,
        ..new_match_request(state, match_id, chapter, time)
    }
}
```

**Why two functions for one message.** `ni.v1.NewMatchRequest.turn` means "0 =
start here, non-zero = resume here". Encoding that in the type system — two
named constructors instead of one function with a `turn` argument — makes the
call sites read as intent (`resume_match_request`) rather than as arithmetic.

`..new_match_request(...)` is *struct update syntax*: build the rest of the
message from that call, then override `turn`. Field order in the literal does
not matter; the `..base` must come last.

**Why `battlefield_view` does not change.** It has a `time_remaining_ms`
field, and it is tempting to put the per-turn budget there. Don't. That field
is for a whole-match budget, which Ni does not have yet, and duplicating the
per-turn deadline into the message body would create two sources of truth for
one number. The deadline belongs in the gRPC metadata, where the transport
itself will enforce it.

### 2.2 Add the test

Append inside `mod tests`:

```rust
    #[test]
    fn a_fresh_match_starts_at_turn_zero_and_a_resume_does_not() {
        let mut state = ni_game::standard_match(Rules::standard());
        state.turn = 7;
        let time = TimeControl {
            turn_deadline: std::time::Duration::from_millis(250),
            strike_limit: 3,
        };

        let fresh = new_match_request(&state, "m3-demo", GameChapter::A, time);
        let resumed = resume_match_request(&state, "m3-demo", GameChapter::A, time);

        assert_eq!(fresh.turn, 0);
        assert_eq!(resumed.turn, 7);
        assert_eq!(resumed.board, fresh.board);
        assert_eq!(resumed.rules.unwrap().turn_deadline_ms, 250);
    }
```

### 2.3 Keep the M2 loop compiling

You just changed a signature two other call sites use, so the crate no longer
builds. That is normal mid-refactor, and it is worth doing the boring thing
rather than skipping ahead: patch the old call sites, get back to green, then
change behaviour.

In `crates/ni-engine/src/match_runner.rs`, add to the `use crate::{...}` block:

```rust
    policy::TimeControl,
```

and give M2's two `NewMatch` calls the argument they now need:

```rust
    bot_a
        .client
        .new_match(new_match_request(
            &state,
            &options.match_id,
            Chapter::A,
            TimeControl::standard(),
        ))
        .await?;

    bot_b
        .client
        .new_match(new_match_request(
            &state,
            &options.match_id,
            Chapter::B,
            TimeControl::standard(),
        ))
        .await?;
```

Checkpoint 4 replaces this whole file, so these four lines are scaffolding —
but scaffolding that keeps the tests runnable in between, which is what makes
a refactor reversible.

### 2.4 Run the checkpoint

```sh
cargo fmt
cargo test -p ni-engine --lib convert
```

Expected: 5 passed.


---

## Checkpoint 3 — a bot process that cannot hang the engine

`BotProcess::spawn` in M2 awaits the readiness line forever. `/bin/sleep` from
Checkpoint 0 hung *here*, before a single gRPC call. Fix that first, and add
the two small capabilities the rest of M3 needs: extra command-line arguments
(so Roger can be told how to misbehave) and the child's exit status (so a
crash can be reported with a number).

### 3.1 Replace `crates/ni-engine/src/process.rs`

```rust
//! Spawn, connect to, and reap one bot process.

use std::{ffi::OsStr, path::Path, process::Stdio, time::Duration};

use anyhow::{Context, Result};
use ni_proto::ni::v1::bot_service_client::BotServiceClient;
use tokio::{
    io::{AsyncBufReadExt, BufReader},
    process::{Child, Command},
    task::JoinHandle,
};
use tonic::transport::Channel;

/// How long a bot gets to print its readiness line and accept a connection.
/// M2 waited forever here; a bot that never binds is a hang before the match
/// even starts, which is exactly the class of failure M3 removes.
pub const STARTUP_DEADLINE: Duration = Duration::from_secs(5);

pub struct BotProcess {
    pub client: BotServiceClient<Channel>,
    endpoint: String,
    child: Child,
    stdout_task: JoinHandle<()>,
}

impl BotProcess {
    pub async fn spawn(path: &Path, label: &str) -> Result<Self> {
        Self::spawn_with_args::<&OsStr>(path, label, &[]).await
    }

    /// `extra_args` is what makes `roger-the-shrubber` usable: the engine
    /// passes `--listen` and the caller passes whatever mischief it wants.
    pub async fn spawn_with_args<S: AsRef<OsStr>>(
        path: &Path,
        label: &str,
        extra_args: &[S],
    ) -> Result<Self> {
        let mut child = Command::new(path)
            .arg("--listen")
            .arg("tcp://127.0.0.1:0")
            .args(extra_args)
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .kill_on_drop(true)
            .spawn()
            .with_context(|| format!("failed to spawn {label} from {}", path.display()))?;

        let setup: Result<_> = async {
            let stdout = child.stdout.take().context("bot stdout was not piped")?;

            let mut lines = BufReader::new(stdout).lines();

            // tokio::time::timeout wraps any future in a deadline. This one
            // covers the readiness line and the dial together, because both
            // are "is this bot alive yet?".
            let ready = tokio::time::timeout(STARTUP_DEADLINE, lines.next_line())
                .await
                .with_context(|| {
                    format!(
                        "{label} printed no readiness line within {}ms",
                        STARTUP_DEADLINE.as_millis()
                    )
                })?
                .context("failed to read bot stdout")?
                .context("bot exited before its readiness line")?;

            let address = ready
                .strip_prefix("LISTENING tcp://")
                .context("invalid bot readiness line")?
                .to_string();

            let client = tokio::time::timeout(
                STARTUP_DEADLINE,
                BotServiceClient::connect(format!("http://{address}")),
            )
            .await
            .with_context(|| format!("{label} did not accept a connection at {address}"))?
            .with_context(|| format!("failed to connect to {label} at {address}"))?;

            Ok((client, address, lines))
        }
        .await;

        let (client, endpoint, mut lines) = match setup {
            Ok(ready) => ready,
            Err(error) => {
                let _ = child.kill().await;
                let _ = child.wait().await;
                return Err(error);
            }
        };

        let label = label.to_string();
        let stdout_task = tokio::spawn(async move {
            while let Ok(Some(line)) = lines.next_line().await {
                eprintln!("[{label}] {line}");
            }
        });

        Ok(Self {
            client,
            endpoint,
            child,
            stdout_task,
        })
    }

    pub fn id(&self) -> Option<u32> {
        self.child.id()
    }

    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// `Some(status)` once the process is gone. Non-blocking, so the engine
    /// can ask "did it crash?" while deciding what a failed call meant.
    pub fn exit_status(&mut self) -> Result<Option<i32>> {
        match self.child.try_wait()? {
            Some(status) => Ok(Some(status.code().unwrap_or(-1))),
            None => Ok(None),
        }
    }

    pub async fn shutdown(&mut self) -> Result<()> {
        if self.child.try_wait()?.is_none() {
            self.child.kill().await?;
        }

        let _ = self.child.wait().await;
        let _ = (&mut self.stdout_task).await;
        Ok(())
    }
}
```

### 3.2 What the Rust is doing

```rust
let ready = tokio::time::timeout(STARTUP_DEADLINE, lines.next_line())
    .await
    .with_context(|| format!("{label} printed no readiness line within ..."))?
    .context("failed to read bot stdout")?
    .context("bot exited before its readiness line")?;
```

Three `?` in a row, because there are three ways to not get a line:

1. `timeout(..)` returns `Result<_, Elapsed>` — the bot is alive but silent;
2. the inner `next_line()` returns `io::Result<Option<String>>` — the pipe
   broke;
3. that `Option` is `None` — the process exited cleanly without saying
   anything.

Each layer gets its own `context` message, so the error the user sees names
the actual failure instead of "operation failed".

```rust
pub async fn spawn_with_args<S: AsRef<OsStr>>(path: &Path, label: &str, extra_args: &[S])
```

`AsRef<OsStr>` is the "anything that can be seen as an OS string" bound, so
callers can pass `&[&str]` (the tests) or `&[String]` (the CLI) without
converting. The cost is that an *empty* slice has no element type to infer,
which is why `spawn` says:

```rust
Self::spawn_with_args::<&OsStr>(path, label, &[]).await
```

The `::<&OsStr>` is a turbofish: you are telling the compiler which `S` to
use, since the empty slice cannot tell it.

`exit_status` wraps `try_wait`, which asks "has this child exited?" without
blocking. The engine calls it *after* a failed request, to turn "transport
error" into "transport error (process exited with code 101)" — the difference
between a diagnosis and a shrug.

### 3.3 Run the checkpoint

```sh
cargo fmt
cargo check -p ni-engine --lib
```

Expected: it compiles. Nothing observable changes yet.


---

## Checkpoint 4 — the match loop that survives its opponents

This is the milestone. Replace `crates/ni-engine/src/match_runner.rs` entirely
— it is easier to read as a whole than as a diff — then walk through it in
five pieces.

### 4.1 The header and the turn outcome

```rust
// The authoritative M3 match loop: deadlines, strikes, recovery, forfeits.

use std::time::{Duration, Instant};

use crate::{
    convert::{battlefield_view, new_match_request, order_from_proto, resume_match_request},
    policy::{
        classify, describe, CallFailure, ForfeitReason, MatchConclusion, Strikes, TimeControl,
    },
    process::BotProcess,
    render::{render_board, render_forfeited_turn, render_result, render_turn},
};
use anyhow::{ensure, Result};
use ni_game::{Chapter, MatchState, MatchStatus, Order, Rules};
use ni_proto::{
    ni::v1::{GetOrdersRequest, GetOrdersResponse, IdentifyRequest, MatchEndedRequest},
    PROTOCOL_VERSION,
};
use tonic::{Request, Status};

/// Deadline for the calls that are not the hot path. They are not the
/// interesting ones, but they still get a clock: an engine that can hang is
/// an engine that will hang.
const SETUP_DEADLINE: Duration = Duration::from_secs(5);

pub struct RunOptions {
    pub delay: Duration,
    pub quiet: bool,
    pub match_id: String,
    pub time: TimeControl,
}

/// What one turn's worth of calling a bot produced, after policy.
enum TurnOutcome {
    /// The bot answered in time, and the echo checked out.
    Orders(Vec<Order>),
    /// This turn is lost, the match continues, the chapter takes a strike.
    TurnForfeited {
        reason: ForfeitReason,
        detail: String,
    },
    /// No point continuing: the bot is gone or unrecoverable.
    MatchForfeited {
        reason: ForfeitReason,
        detail: String,
    },
}
```

`TurnOutcome` is the shape of the whole design: after policy has run, a turn
produced *orders*, or *cost this turn*, or *ended the match*. Note what is
missing — there is no `Err`. A bot behaving badly is a game event, not an
engine error, and the type says so before any code does.

`SETUP_DEADLINE` covers the calls that are not the hot path. They are boring,
but "boring" is how you get a hang: an engine with one unbounded `await` in it
will eventually stop on that line.

### 4.2 `run_match`

```rust
pub async fn run_match(
    bot_a: &mut BotProcess,
    bot_b: &mut BotProcess,
    options: RunOptions,
) -> Result<MatchConclusion> {
    identify(bot_a, "A").await?;
    identify(bot_b, "B").await?;

    let mut state = ni_game::standard_match(Rules::standard());

    new_match(bot_a, &state, Chapter::A, &options).await?;
    new_match(bot_b, &state, Chapter::B, &options).await?;

    if !options.quiet {
        println!(
            "match {} begins (deadline {}ms, {} strikes)",
            options.match_id,
            options.time.turn_deadline_ms(),
            options.time.strike_limit
        );
        print!("{}", render_board(&state));
    }

    let mut strikes_a = Strikes::default();
    let mut strikes_b = Strikes::default();

    let conclusion = loop {
        let acting = state.to_act;
        let turn = state.turn;

        let (bot, strikes) = match acting {
            Chapter::A => (&mut *bot_a, &mut strikes_a),
            Chapter::B => (&mut *bot_b, &mut strikes_b),
        };

        match request_orders(bot, &state, acting, &options).await {
            TurnOutcome::Orders(orders) => {
                strikes.clear();

                let (next_state, outcomes) = ni_game::apply_orders(state, acting, &orders);
                state = next_state;

                if !options.quiet {
                    print!("{}", render_turn(&state, acting, turn, &outcomes));
                }
            }
            TurnOutcome::TurnForfeited { reason, detail } => {
                strikes.record(reason);
                let strikes = *strikes;

                if !options.quiet {
                    print!(
                        "{}",
                        render_forfeited_turn(
                            turn,
                            acting,
                            reason,
                            &detail,
                            strikes,
                            options.time.strike_limit
                        )
                    );
                }

                if let Some(reason) = strikes.exhausted(options.time.strike_limit) {
                    break MatchConclusion::Forfeit {
                        loser: acting,
                        reason,
                    };
                }
            }
            TurnOutcome::MatchForfeited { reason, detail } => {
                if !options.quiet {
                    println!("turn {turn}: chapter {acting:?} cannot continue ({detail})");
                }

                break MatchConclusion::Forfeit {
                    loser: acting,
                    reason,
                };
            }
        }

        match ni_game::match_status(&state) {
            MatchStatus::InProgress => {}
            finished => break MatchConclusion::Decided(finished),
        }

        ni_game::end_turn(&mut state);

        match ni_game::match_status(&state) {
            MatchStatus::InProgress => {}
            finished => break MatchConclusion::Decided(finished),
        }

        if !options.delay.is_zero() {
            tokio::time::sleep(options.delay).await;
        }
    };

    notify_match_ended(bot_a, &options.match_id, Chapter::A, conclusion).await;
    notify_match_ended(bot_b, &options.match_id, Chapter::B, conclusion).await;

    println!("{}", render_result(conclusion));
    Ok(conclusion)
}
```

Read the loop as four sentences:

1. **Pick the actor.** `(&mut *bot_a, &mut strikes_a)` reborrows both the
   process and its counter, so the same code path handles either chapter.
   Strikes are per chapter because they are a statement about *that bot*.
2. **Ask for orders, and take what policy gives back.** Orders clear the
   strikes — one good turn wipes the slate, which is what makes the counter
   mean "has stopped answering" rather than "has ever been slow".
3. **A forfeited turn still spends the turn.** Nothing is applied, the board
   is unchanged, and the loop falls through to `end_turn`. That matters: the
   opponent gets to act, the turn cap still advances, and a permanently broken
   bot cannot freeze the match — it loses it.
4. **Break with a conclusion, never with `?`.** Both `break` sites produce a
   `MatchConclusion`, so the caller always gets a result.

`let strikes = *strikes;` copies the counter out of the mutable borrow after
recording, so the renderer can read it while nothing else holds a borrow.
`Strikes` is `Copy`, which makes that a two-word copy rather than a clone.

The status checks before and after `end_turn` are M2's, unchanged: the first
catches elimination, the second catches the turn cap.

### 4.3 The policy layer: one call, every reaction

```rust
/// One `GetOrders`, plus everything the engine is willing to do about it
/// failing. Returns a decision, never an error: a misbehaving bot is a game
/// outcome, not an engine bug.
async fn request_orders(
    bot: &mut BotProcess,
    state: &MatchState,
    acting: Chapter,
    options: &RunOptions,
) -> TurnOutcome {
    let status = match call_get_orders(bot, state, acting, options).await {
        Ok(response) => return orders_or_protocol_error(response, state.turn),
        Err(status) => status,
    };

    match classify(&status) {
        CallFailure::Timeout => TurnOutcome::TurnForfeited {
            reason: ForfeitReason::Timeout,
            detail: format!(
                "no answer within {}ms ({})",
                options.time.turn_deadline_ms(),
                describe(&status)
            ),
        },
        CallFailure::NeedsShrubbery => recover_and_retry(bot, state, acting, options).await,
        CallFailure::Unreachable(detail) => TurnOutcome::MatchForfeited {
            reason: ForfeitReason::Crash,
            detail: with_exit_status(bot, detail),
        },
        CallFailure::Protocol(detail) => TurnOutcome::TurnForfeited {
            reason: ForfeitReason::Protocol,
            detail,
        },
    }
}

/// The `SHRUBBERY_REQUIRED` recovery: re-send `NewMatch` at the current turn,
/// then ask again. Safe to repeat because the turn number travels in both
/// directions — a doubled call for turn 12 is still a call for turn 12.
async fn recover_and_retry(
    bot: &mut BotProcess,
    state: &MatchState,
    acting: Chapter,
    options: &RunOptions,
) -> TurnOutcome {
    if !options.quiet {
        println!(
            "turn {}: chapter {acting:?} has not been brought a shrubbery; \
             re-sending NewMatch and retrying",
            state.turn
        );
    }

    let mut request = Request::new(resume_match_request(
        state,
        &options.match_id,
        acting,
        options.time,
    ));
    request.set_timeout(SETUP_DEADLINE);

    if let Err(status) = bot.client.new_match(request).await {
        return match classify(&status) {
            CallFailure::Unreachable(detail) => TurnOutcome::MatchForfeited {
                reason: ForfeitReason::Crash,
                detail: with_exit_status(bot, detail),
            },
            _ => TurnOutcome::MatchForfeited {
                reason: ForfeitReason::Protocol,
                detail: format!("rejected the replacement NewMatch: {}", describe(&status)),
            },
        };
    }

    let status = match call_get_orders(bot, state, acting, options).await {
        Ok(response) => return orders_or_protocol_error(response, state.turn),
        Err(status) => status,
    };

    match classify(&status) {
        CallFailure::Timeout => TurnOutcome::TurnForfeited {
            reason: ForfeitReason::Timeout,
            detail: format!("no answer after recovery ({})", describe(&status)),
        },
        CallFailure::Unreachable(detail) => TurnOutcome::MatchForfeited {
            reason: ForfeitReason::Crash,
            detail: with_exit_status(bot, detail),
        },
        // Twice in a row means the bot never actually accepted the match.
        // Retrying a third time would be a loop, not a recovery.
        CallFailure::NeedsShrubbery => TurnOutcome::MatchForfeited {
            reason: ForfeitReason::Protocol,
            detail: "demanded a shrubbery again after NewMatch".to_string(),
        },
        CallFailure::Protocol(detail) => TurnOutcome::TurnForfeited {
            reason: ForfeitReason::Protocol,
            detail,
        },
    }
}
```

**Timeout → the turn, not the match.** The engine does *not* retry. It could:
the turn echo would make a retry safe against double-application. It does not,
because the honest answer to "did the bot decide?" after a deadline is *you
cannot know*, and blog post 4 is about that ambiguity. Retrying here would
paper over the interesting part; forfeiting the turn names it.

**`SHRUBBERY_REQUIRED` → re-send and retry once.** This is the one place a
retry is unambiguous. The engine holds all the truth, so recovery is: send
`NewMatch` again with the resume turn, then re-ask for the same turn. The
retry cannot double-apply because nothing was applied the first time — the bot
never produced orders — and even if the first call *had* landed, the echoed
turn number means the engine can tell.

**Twice in a row is not recovery, it is a loop.** The second
`NeedsShrubbery` becomes a match forfeit. Any retry policy needs a stopping
rule, and "once" is the smallest one that works.

**Unreachable → the match, immediately.** No strikes: strikes exist to
tolerate transient slowness, and a dead process is not transient. The exit
code is attached to the message via `with_exit_status`.

**A broken answer → the turn.** Both a bad status and a wrong echo are the
bot's fault but not fatal — three in a row still ends the match, through the
same counter.

### 4.4 The one line that sets the deadline

```rust
/// The only place the per-turn deadline is set. `set_timeout` writes the
/// `grpc-timeout` header, so the bot is *told* how long it has rather than
/// being cut off by a client-side stopwatch it cannot see.
async fn call_get_orders(
    bot: &mut BotProcess,
    state: &MatchState,
    acting: Chapter,
    options: &RunOptions,
) -> Result<GetOrdersResponse, Status> {
    let mut request = Request::new(GetOrdersRequest {
        match_id: options.match_id.clone(),
        turn: state.turn,
        view: Some(battlefield_view(state)),
    });
    request.set_timeout(options.time.turn_deadline);

    let started = Instant::now();
    let result = bot.client.get_orders(request).await;
    let elapsed = started.elapsed();

    if !options.quiet {
        println!(
            "turn {}: GetOrders -> chapter {acting:?} {} in {}ms",
            state.turn,
            match &result {
                Ok(_) => "answered".to_string(),
                Err(status) => format!("failed with {:?}", status.code()),
            },
            elapsed.as_millis()
        );
    }

    result.map(|response| response.into_inner())
}

/// The turn echo is the whole retry story in one `if`. A response for a turn
/// the engine is not playing is not orders — it is noise from an earlier
/// call, or a bot that is confused. Either way it must not be applied.
fn orders_or_protocol_error(response: GetOrdersResponse, turn: u32) -> TurnOutcome {
    if response.turn != turn {
        return TurnOutcome::TurnForfeited {
            reason: ForfeitReason::Protocol,
            detail: format!(
                "echoed turn {} while the engine plays {turn}",
                response.turn
            ),
        };
    }

    TurnOutcome::Orders(response.orders.into_iter().map(order_from_proto).collect())
}

fn with_exit_status(bot: &mut BotProcess, detail: String) -> String {
    match bot.exit_status() {
        Ok(Some(code)) => format!("{detail} (process exited with code {code})"),
        _ => format!("{detail} (process still running)"),
    }
}
```

`request.set_timeout(options.time.turn_deadline)` writes a `grpc-timeout`
header. This is the difference blog post 3 is built on: a client-side
stopwatch abandons the call and leaves the server working on a result nobody
wants, while a deadline is *propagated* — the bot's own server knows how long
the caller will wait, and can pass that budget on to anything it calls.

The stopwatch around the call is not for the deadline (the transport handles
that); it is for the log line. `elapsed` on a successful call is the number
M4's match log and blog post 3's spans will care about, and having it printed
from day one is how you notice that a "500ms" deadline is answering in 1ms.

`orders_or_protocol_error` is where M2's `ensure!` used to be. The change from
"abort the match" to "forfeit this turn" is small in code and large in
behaviour: a bot that echoes turn 3 when the engine is playing turn 2 is
answering a question nobody asked, and applying those orders would be applying
a stale decision to a board that has moved on.

### 4.5 The supporting calls

```rust
async fn identify(bot: &mut BotProcess, label: &str) -> Result<()> {
    let mut request = Request::new(IdentifyRequest {
        protocol_version: PROTOCOL_VERSION,
    });
    request.set_timeout(SETUP_DEADLINE);

    let identity = bot.client.identify(request).await?.into_inner();

    ensure!(
        identity.protocol_version == PROTOCOL_VERSION,
        "bot {label} ({}) speaks protocol {}, engine requires {}",
        identity.name,
        identity.protocol_version,
        PROTOCOL_VERSION
    );

    println!(
        "bot {label}: pid={}, tcp://{}, {} {} (protocol {})",
        bot.id()
            .map(|id| id.to_string())
            .unwrap_or_else(|| "unknown".to_string()),
        bot.endpoint(),
        identity.name,
        identity.version,
        identity.protocol_version,
    );

    Ok(())
}

async fn new_match(
    bot: &mut BotProcess,
    state: &MatchState,
    chapter: Chapter,
    options: &RunOptions,
) -> Result<()> {
    let mut request = Request::new(new_match_request(
        state,
        &options.match_id,
        chapter,
        options.time,
    ));
    request.set_timeout(SETUP_DEADLINE);

    bot.client.new_match(request).await?;
    Ok(())
}

async fn notify_match_ended(
    bot: &mut BotProcess,
    match_id: &str,
    chapter: Chapter,
    conclusion: MatchConclusion,
) {
    let mut request = Request::new(MatchEndedRequest {
        match_id: match_id.to_string(),
        outcome: conclusion.outcome_for(chapter),
        reason: conclusion.end_reason(),
    });
    request.set_timeout(SETUP_DEADLINE);

    // Best effort by definition: the most likely reason a match ended is
    // that this bot stopped answering.
    if let Err(error) = bot.client.match_ended(request).await {
        eprintln!("could not notify bot {chapter:?} that the match ended: {error}");
    }
}
```

These three keep `?` and `Result`, and that is deliberate: a bot that cannot
complete the handshake has not started a match, so there is nothing to forfeit
— that is a runner error, and the CLI should say so and exit.

`notify_match_ended` is best-effort by construction. The most common reason a
match ended is that this very bot stopped answering, so the notification
failing is expected, and its failure must not overwrite the result.

### 4.6 Compile

```sh
cargo fmt
cargo check -p ni-engine --lib
```

Expected: errors about `render_forfeited_turn` and `render_result`. That is
the next checkpoint.


---

## Checkpoint 5 — render the new outcomes

A forfeit that is not visible is a forfeit you will not debug.

### 5.1 Edit `crates/ni-engine/src/render.rs`

Add the import:

```rust
use crate::policy::{ForfeitReason, MatchConclusion, Strikes};
```

Replace `render_result` and add `render_forfeited_turn` and `forfeit_name`:

```rust
/// A turn nobody played. The strike count is shown because it is the only
/// thing standing between this turn and the end of the match.
pub fn render_forfeited_turn(
    turn: u32,
    acting: Chapter,
    reason: ForfeitReason,
    detail: &str,
    strikes: Strikes,
    limit: u32,
) -> String {
    format!(
        "turn {turn}: chapter {} forfeits the turn — {} ({detail}); strike {}/{limit}\n",
        chapter_name(acting),
        forfeit_name(reason),
        strikes.count(),
    )
}

pub fn render_result(conclusion: MatchConclusion) -> String {
    match conclusion {
        MatchConclusion::Decided(MatchStatus::InProgress) => {
            "result: match still in progress".to_string()
        }
        MatchConclusion::Decided(MatchStatus::Winner { chapter, reason }) => format!(
            "result: chapter {} wins by {}",
            chapter_name(chapter),
            reason_name(reason)
        ),
        MatchConclusion::Decided(MatchStatus::Draw { reason }) => {
            format!("result: draw by {}", reason_name(reason))
        }
        MatchConclusion::Forfeit { loser, reason } => format!(
            "result: chapter {} wins — chapter {} forfeits ({})",
            chapter_name(loser.opponent()),
            chapter_name(loser),
            forfeit_name(reason)
        ),
    }
}

fn forfeit_name(reason: ForfeitReason) -> &'static str {
    match reason {
        ForfeitReason::Timeout => "missed deadline",
        ForfeitReason::Crash => "unreachable",
        ForfeitReason::Protocol => "broken contract",
    }
}
```

The strike count is in the line because it is the only thing standing between
this turn and the end of the match. `strike 2/3` tells you the next one
matters; `strike 1/3` tells you to keep watching.

### 5.2 Add the tests

Inside `mod tests`:

```rust
    #[test]
    fn a_forfeited_match_reads_as_a_win_for_the_other_chapter() {
        let text = render_result(MatchConclusion::Forfeit {
            loser: Chapter::B,
            reason: ForfeitReason::Timeout,
        });

        assert!(text.contains("chapter A wins"), "{text}");
        assert!(text.contains("missed deadline"), "{text}");
    }

    #[test]
    fn a_forfeited_turn_shows_the_strike_count() {
        let mut strikes = Strikes::default();
        strikes.record(ForfeitReason::Timeout);

        let text = render_forfeited_turn(
            4,
            Chapter::A,
            ForfeitReason::Timeout,
            "no answer within 250ms",
            strikes,
            3,
        );

        assert!(text.contains("turn 4"), "{text}");
        assert!(text.contains("strike 1/3"), "{text}");
    }
```

### 5.3 Run the checkpoint

```sh
cargo fmt
cargo test -p ni-engine --lib
```

Expected: 15 passed (policy, convert, render).


---

## Checkpoint 6 — the CLI gets a clock

### 6.1 Replace `crates/ni-engine/src/main.rs`

```rust
use std::{path::PathBuf, time::Duration};

use anyhow::Result;
use clap::{Parser, Subcommand};
use ni_engine::{run_match, BotProcess, MatchConclusion, RunOptions, TimeControl};

#[derive(Parser)]
#[command(about = "Run authoritative Ni matches")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    Run {
        #[arg(long)]
        bot_a: PathBuf,

        #[arg(long)]
        bot_b: PathBuf,

        /// Extra argument passed to bot A. Repeat for several:
        /// `--bot-a-arg --sleep-ms --bot-a-arg 400`.
        #[arg(long = "bot-a-arg", allow_hyphen_values = true)]
        bot_a_arg: Vec<String>,

        #[arg(long = "bot-b-arg", allow_hyphen_values = true)]
        bot_b_arg: Vec<String>,

        #[arg(
            long,
            default_value= "0ms",
            value_parser = parse_duration
        )]
        delay: Duration,

        /// How long a bot may think about one turn.
        #[arg(
            long,
            default_value = "500ms",
            value_parser = parse_duration
        )]
        turn_deadline: Duration,

        /// Consecutive forfeited turns before the match is forfeited.
        #[arg(long, default_value_t = 3)]
        strike_limit: u32,

        #[arg(long)]
        quiet: bool,
    },
}

fn parse_duration(value: &str) -> std::result::Result<Duration, String> {
    let milliseconds = value
        .strip_suffix("ms")
        .ok_or_else(|| "duration mus end in ms, for example 200ms".to_string())?
        .parse::<u64>()
        .map_err(|error| format!("invalid millisecond duration: {error}"))?;

    Ok(Duration::from_millis(milliseconds))
}

#[tokio::main]
async fn main() -> Result<()> {
    let Command::Run {
        bot_a,
        bot_b,
        bot_a_arg,
        bot_b_arg,
        delay,
        turn_deadline,
        strike_limit,
        quiet,
    } = Cli::parse().command;

    let options = RunOptions {
        delay,
        quiet,
        match_id: "m3-demo".to_string(),
        time: TimeControl {
            turn_deadline,
            strike_limit,
        },
    };

    let conclusion = run(bot_a, bot_a_arg, bot_b, bot_b_arg, options).await?;

    // A forfeit is a legitimate match result, not a failure of the engine —
    // so the process still exits 0. The exit code says "the engine worked".
    if let MatchConclusion::Forfeit { loser, reason } = conclusion {
        eprintln!("note: chapter {loser:?} forfeited ({reason:?})");
    }

    Ok(())
}

async fn run(
    bot_a_path: PathBuf,
    bot_a_args: Vec<String>,
    bot_b_path: PathBuf,
    bot_b_args: Vec<String>,
    options: RunOptions,
) -> Result<MatchConclusion> {
    let mut bot_a = BotProcess::spawn_with_args(&bot_a_path, "bot A", &bot_a_args).await?;

    let mut bot_b = match BotProcess::spawn_with_args(&bot_b_path, "bot B", &bot_b_args).await {
        Ok(bot) => bot,
        Err(error) => {
            if let Err(cleanup_error) = bot_a.shutdown().await {
                eprintln!(
                    "could not clean up bot A after bot B failed to start: \
                     {cleanup_error}"
                );
            }
            return Err(error);
        }
    };

    let match_result = run_match(&mut bot_a, &mut bot_b, options).await;

    let cleanup_a = bot_a.shutdown().await;
    let cleanup_b = bot_b.shutdown().await;

    let conclusion = match_result?;
    cleanup_a?;
    cleanup_b?;
    Ok(conclusion)
}
```

Three things to notice.

**`allow_hyphen_values = true`.** Without it, `--bot-b-arg --sleep-ms` fails
with *unexpected argument '--sleep-ms' found*: clap assumes a value starting
with `-` is a flag you mistyped. The attribute says "this option's value may
look like a flag", which is exactly what passing arguments through to a child
process requires.

**Destructuring in the `let`.** `let Command::Run { .. } = Cli::parse().command;`
works without a `match` because `Command` has a single variant — an
*irrefutable* pattern. Add a second subcommand later and the compiler will
demand a `match` again.

**Exit code 0 on a forfeit.** The engine ran correctly; a chapter lost. The
note goes to stderr so a tournament script can see it without it polluting the
result on stdout. If forfeits exited non-zero, every CI run with a slow
machine would look like a broken engine.

### 6.2 Run a real match with a deadline

```sh
cargo fmt
cargo build --workspace
./target/debug/ni-engine run \
  --bot-a target/debug/ni-bot \
  --bot-b target/debug/ni-bot \
  --turn-deadline 200ms
```

Expected — the new latency line before each turn, and the same result M2 gave:

```text
match m3-demo begins (deadline 200ms, 3 strikes)
turn 1: GetOrders -> chapter A answered in 1ms
turn 1: chapter A acted
...
result: chapter A wins by elimination
```

The reference bot answers in about a millisecond, so nothing forfeits. To see
the rest of the milestone work, you need an opponent that does not.


---

## Checkpoint 7 — share the bot scaffolding

Roger needs the same listener, the same readiness line and the same tonic
server as the reference bot; only his *answers* differ. Copying forty lines
into a second crate would mean two places to fix when the transport changes in
M5 — so lift them into `ni-bot`'s library first.

### 7.1 Create `crates/ni-bot/src/server.rs`

```rust
//! Listener and server scaffolding shared by every bot in this workspace.
//!
//! The reference bot and `roger-the-shrubber` differ only in what they answer
//! — binding, the readiness line and the tonic server are the same code, so
//! they live here rather than being copied.

use std::io::Write as _;

use anyhow::{Context, Result};
use ni_proto::ni::v1::bot_service_server::{BotService, BotServiceServer};
use tokio::net::TcpListener;
use tokio_stream::wrappers::TcpListenerStream;
use tonic::transport::Server;

/// Bind `listen`, announce the real address on stdout, and serve `service`
/// until the process is killed.
///
/// The readiness line is a contract with the engine, not gRPC: the engine
/// reads exactly one line, so it must be printed *and flushed* before the
/// server starts.
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
        .serve_with_incoming(TcpListenerStream::new(listener))
        .await?;

    Ok(())
}
```

`S: BotService` is the generated service trait, so `serve` accepts any
implementation of the contract — `ReferenceBot` today, `Roger` in two
checkpoints, a Python-shaped stub never (that one gets its own process). The
function is generic, so the tonic server is built once per concrete bot at
compile time; there is no dynamic dispatch and no `Box`.

The `flush` matters more than it looks. `println!` to a pipe is buffered, and
the engine blocks reading exactly one line — an unflushed readiness line is a
deadlock that only appears when stdout is not a terminal, which is to say only
under the engine and never when you test the bot by hand.

### 7.2 Edit `crates/ni-bot/src/lib.rs`

Add at the very top, above the existing `use` statements:

```rust
pub mod server;

pub use server::serve;
```

Nothing else in that file changes.

### 7.3 Replace `crates/ni-bot/src/main.rs`

```rust
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
```

The binary is now seventeen lines: parse a flag, serve a service. Everything
that was in it is either shared (`server.rs`) or the strategy (`lib.rs`).

### 7.4 Run the checkpoint

```sh
cargo fmt
cargo test -p ni-bot
cargo build -p ni-bot
```

Expected: the bot's own tests still pass and the binary still starts. Confirm
by hand:

```sh
./target/debug/ni-bot --listen tcp://127.0.0.1:0
```

It prints `LISTENING tcp://127.0.0.1:<port>` and waits. `Ctrl-C` it.


---

## Checkpoint 8 — build `roger-the-shrubber`

> "We are now the Knights Who Say... Ni-except-that-request-timed-out."

Roger is a test fixture with a socket. Every failure the engine claims to
survive needs something that causes it on demand, at a specific turn, the same
way every run — otherwise your failure handling is a story you tell yourself.

### 8.1 Register the crate

In the workspace `Cargo.toml`, add the member and one workspace dependency:

```toml
[workspace]
resolver = "2"
members = [
    "crates/ni-proto",
    "crates/ni-game",
    "crates/ni-engine",
    "crates/ni-bot",
    "crates/roger-the-shrubber",
]
```

and, under `[workspace.dependencies]`, next to the other path dependencies:

```toml
ni-bot = { path = "crates/ni-bot" }
```

### 8.2 Create `crates/roger-the-shrubber/Cargo.toml`

```toml
[package]
name = "roger-the-shrubber"
version.workspace = true
edition.workspace = true
repository.workspace = true
description = "A deliberately hostile Ni bot: sleeps, crashes, forgets matches, returns nonsense"

[dependencies]
ni-bot = { workspace = true }
ni-proto = { workspace = true }
anyhow = { workspace = true }
clap = { workspace = true }
tokio = { workspace = true }
tonic = { workspace = true }
```

Roger depends on `ni-bot` — the crate whose scaffolding he reuses — but not on
`ni-game`. He is a bot: he sees views and returns orders, and has no access to
the rules. That is not politeness, it is the architecture. A bot that could
call `apply_orders` is a bot that could decide whether its own orders are
legal, and the engine's authority would be a suggestion.

### 8.3 Create `crates/roger-the-shrubber/src/lib.rs`

The flags first:

```rust
//! `roger-the-shrubber`: a bot that misbehaves on purpose.
//!
//! Every failure the M3 engine claims to survive needs something that
//! actually causes it. Roger is that something: he is slow, he forgets
//! matches, he orders knights he does not own, he answers about the wrong
//! turn, and — if asked — he dies mid-call.
//!
//! He is a *test fixture with a socket*. The strategy itself is borrowed
//! from the reference bot, so a Roger with no flags plays a normal match.

use std::{
    collections::HashMap,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use clap::Args;
use ni_bot::choose_order;
use ni_proto::{
    ni::v1::{
        bot_service_server::BotService, BattlefieldView, Chapter, GetOrdersRequest,
        GetOrdersResponse, IdentifyRequest, IdentifyResponse, KnightOrder, MatchEndedRequest,
        MatchEndedResponse, NewMatchRequest, NewMatchResponse, Position, SubmitReplayRequest,
        SubmitReplayResponse,
    },
    PROTOCOL_VERSION, SHRUBBERY_REQUIRED,
};
use tokio::sync::RwLock;
use tonic::{Request, Response, Status};

/// Which misbehaviours are switched on. Everything is off by default, so
/// `roger-the-shrubber` with no flags is a (slightly dim) normal opponent.
#[derive(Args, Clone, Copy, Debug, Default)]
pub struct Mischief {
    /// Think this long before answering `GetOrders`.
    #[arg(long, default_value_t = 0)]
    pub sleep_ms: u64,

    /// Restrict `--sleep-ms` to this turn only. Without it, every turn is
    /// slow — which is how you reach the strike limit.
    #[arg(long)]
    pub sleep_on_turn: Option<u32>,

    /// Exit the process (uncleanly) when this turn arrives.
    #[arg(long)]
    pub crash_on_turn: Option<u32>,

    /// Forget every match once, when this turn arrives: the next `GetOrders`
    /// answers `SHRUBBERY_REQUIRED`, like a restarted daemon would.
    #[arg(long)]
    pub forget_on_turn: Option<u32>,

    /// Order a knight to a tile far off the board.
    #[arg(long)]
    pub illegal_orders: bool,

    /// Order the *opponent's* knights around.
    #[arg(long)]
    pub steal_knights: bool,

    /// Echo the wrong turn number in the response.
    #[arg(long)]
    pub wrong_turn: bool,
}
```

Each flag exists to reach one branch of `classify`: `--sleep-ms` reaches
`Timeout`, `--crash-on-turn` reaches `Unreachable`, `--forget-on-turn` reaches
`NeedsShrubbery`, `--wrong-turn` reaches the echo check, and
`--illegal-orders` / `--steal-knights` reach `ni-game`'s existing
`IllegalReason`s without any status code at all.

`--sleep-on-turn` is the difference between "misses one deadline" and "misses
every deadline", which is the difference between a strike and a forfeit. You
need both to prove the counter resets.

Now the state:

```rust
pub struct Roger {
    mischief: Mischief,
    matches: RwLock<HashMap<String, Chapter>>,
    /// `forget_on_turn` fires once. Without this latch the bot would forget
    /// the match again on the retry, and recovery could never converge.
    already_forgot: AtomicBool,
}

impl Roger {
    pub fn new(mischief: Mischief) -> Self {
        Self {
            mischief,
            matches: RwLock::new(HashMap::new()),
            already_forgot: AtomicBool::new(false),
        }
    }

    async fn maybe_forget(&self, turn: u32) {
        if self.mischief.forget_on_turn != Some(turn) {
            return;
        }

        if self.already_forgot.swap(true, Ordering::SeqCst) {
            return;
        }

        eprintln!("roger: forgetting every match at turn {turn}");
        self.matches.write().await.clear();
    }

    async fn maybe_sleep(&self, turn: u32) {
        let wanted = match self.mischief.sleep_on_turn {
            Some(target) => target == turn,
            None => self.mischief.sleep_ms > 0,
        };

        if wanted && self.mischief.sleep_ms > 0 {
            eprintln!(
                "roger: sleeping {}ms on turn {turn}",
                self.mischief.sleep_ms
            );
            tokio::time::sleep(Duration::from_millis(self.mischief.sleep_ms)).await;
        }
    }

    fn maybe_crash(&self, turn: u32) {
        if self.mischief.crash_on_turn == Some(turn) {
            eprintln!("roger: dying on turn {turn}");
            // Not a panic: a panic in a handler is caught and turned into a
            // status. This kills the process mid-request, which is what a
            // real crash looks like from the engine's side of the socket.
            std::process::exit(101);
        }
    }
}
```

**Why the latch.** Without `already_forgot`, `--forget-on-turn 4` would forget
the match on turn 4, answer `SHRUBBERY_REQUIRED`, receive the replacement
`NewMatch`, and then forget it again on the retry — which is still turn 4.
The engine would give up after one retry (correctly), and you would never
exercise the *successful* recovery path. One-shot misbehaviour is what makes a
recovery test a recovery test.

**Why `std::process::exit(101)` and not `panic!`.** tonic catches a panicking
handler and turns it into an `Internal` status: the process survives, the
connection survives, and the engine sees a well-behaved error. That is a
useful case, but it is not a crash. Exiting kills the process mid-request,
which is what the engine sees when a bot segfaults, gets OOM-killed, or is
`kill -9`'d.

The service implementation:

```rust
#[tonic::async_trait]
impl BotService for Roger {
    async fn identify(
        &self,
        _request: Request<IdentifyRequest>,
    ) -> Result<Response<IdentifyResponse>, Status> {
        Ok(Response::new(IdentifyResponse {
            name: "roger-the-shrubber".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: PROTOCOL_VERSION,
        }))
    }

    async fn new_match(
        &self,
        request: Request<NewMatchRequest>,
    ) -> Result<Response<NewMatchResponse>, Status> {
        let request = request.into_inner();

        let chapter = Chapter::try_from(request.chapter)
            .map_err(|_| Status::invalid_argument("unknown chapter"))?;

        eprintln!(
            "roger: NewMatch {} as {chapter:?} at turn {}",
            request.match_id, request.turn
        );

        self.matches.write().await.insert(request.match_id, chapter);

        Ok(Response::new(NewMatchResponse {}))
    }

    async fn get_orders(
        &self,
        request: Request<GetOrdersRequest>,
    ) -> Result<Response<GetOrdersResponse>, Status> {
        let request = request.into_inner();

        self.maybe_crash(request.turn);
        self.maybe_forget(request.turn).await;

        let chapter = self
            .matches
            .read()
            .await
            .get(&request.match_id)
            .copied()
            .ok_or_else(|| Status::failed_precondition(SHRUBBERY_REQUIRED))?;

        let view = request
            .view
            .ok_or_else(|| Status::invalid_argument("view is required"))?;

        self.maybe_sleep(request.turn).await;

        let turn = if self.mischief.wrong_turn {
            request.turn.wrapping_add(1)
        } else {
            request.turn
        };

        Ok(Response::new(GetOrdersResponse {
            turn,
            orders: self.orders(&view, chapter),
        }))
    }

    async fn match_ended(
        &self,
        request: Request<MatchEndedRequest>,
    ) -> Result<Response<MatchEndedResponse>, Status> {
        let request = request.into_inner();
        eprintln!(
            "roger: MatchEnded {} outcome={} reason={}",
            request.match_id, request.outcome, request.reason
        );

        self.matches.write().await.remove(&request.match_id);
        Ok(Response::new(MatchEndedResponse {}))
    }

    async fn submit_replay(
        &self,
        _request: Request<SubmitReplayRequest>,
    ) -> Result<Response<SubmitReplayResponse>, Status> {
        Ok(Response::new(SubmitReplayResponse {}))
    }
}
```

The order inside `get_orders` is the script: crash before anything else,
forget before looking up the match (so the lookup fails), sleep after the
lookup (so the delay lands *inside* the deadline window rather than before the
bot knows what it is doing).

Roger's `eprintln!` lines go to stderr, which the engine inherits, so his
confessions interleave with the engine's log in your terminal. When a test
fails, that interleaving is the whole debugging story.

And the orders themselves:

```rust
impl Roger {
    /// Orders are built from the same view every bot gets — the mischief is
    /// in what he does with it, not in what he can see.
    fn orders(&self, view: &BattlefieldView, chapter: Chapter) -> Vec<KnightOrder> {
        if self.mischief.steal_knights {
            if let Some(enemy) = first_living(view, opponent(chapter)) {
                return vec![KnightOrder {
                    unit_id: enemy,
                    move_to: Some(Position { x: 0, y: 0 }),
                    attack_target: None,
                }];
            }
        }

        if self.mischief.illegal_orders {
            if let Some(ally) = first_living(view, chapter) {
                return vec![KnightOrder {
                    unit_id: ally,
                    move_to: Some(Position { x: 999, y: 999 }),
                    attack_target: None,
                }];
            }
        }

        choose_order(view, chapter).into_iter().collect()
    }
}

fn opponent(chapter: Chapter) -> Chapter {
    match chapter {
        Chapter::A => Chapter::B,
        Chapter::B => Chapter::A,
        Chapter::Unspecified => Chapter::Unspecified,
    }
}

fn first_living(view: &BattlefieldView, chapter: Chapter) -> Option<String> {
    view.knights
        .iter()
        .find(|knight| knight.hp > 0 && Chapter::try_from(knight.chapter).ok() == Some(chapter))
        .map(|knight| knight.unit_id.clone())
}
```

Both hostile order shapes are *valid protobuf*. `999, 999` is a legal
`Position`; the enemy's `unit_id` is a legal string. Nothing about the
contract can stop them, which is precisely why the engine — not the schema —
has to be the authority.

### 8.4 Create `crates/roger-the-shrubber/src/main.rs`

```rust
use anyhow::Result;
use clap::Parser;
use ni_bot::serve;
use roger_the_shrubber::{Mischief, Roger};

#[derive(Parser)]
#[command(about = "Run roger-the-shrubber: a deliberately hostile Ni bot")]
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
```

### 8.5 Add Roger's own tests

Append to `lib.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use ni_proto::ni::v1::{BoardLayout, Knight, Rules};
    use tonic::Code;

    fn view(turn: u32) -> BattlefieldView {
        BattlefieldView {
            width: 8,
            height: 1,
            shrubbery: vec![],
            knights: vec![
                Knight {
                    unit_id: "A1".to_string(),
                    chapter: Chapter::A as i32,
                    position: Some(Position { x: 0, y: 0 }),
                    hp: 10,
                    move_range: 3,
                    attack_range: 2,
                    attack_damage: 4,
                },
                Knight {
                    unit_id: "B1".to_string(),
                    chapter: Chapter::B as i32,
                    position: Some(Position { x: 7, y: 0 }),
                    hp: 10,
                    move_range: 3,
                    attack_range: 2,
                    attack_damage: 4,
                },
            ],
            to_act: Chapter::A as i32,
            turn,
            time_remaining_ms: 0,
        }
    }

    async fn started(mischief: Mischief) -> Roger {
        let roger = Roger::new(mischief);

        roger
            .new_match(Request::new(NewMatchRequest {
                match_id: "m3-test".to_string(),
                chapter: Chapter::A as i32,
                board: Some(BoardLayout {
                    width: 8,
                    height: 1,
                    shrubbery: vec![],
                }),
                rules: Some(Rules::default()),
                turn: 0,
            }))
            .await
            .unwrap();

        roger
    }

    async fn ask(roger: &Roger, turn: u32) -> Result<GetOrdersResponse, Status> {
        roger
            .get_orders(Request::new(GetOrdersRequest {
                match_id: "m3-test".to_string(),
                turn,
                view: Some(view(turn)),
            }))
            .await
            .map(|response| response.into_inner())
    }

    #[tokio::test]
    async fn a_flagless_roger_plays_normally() {
        let roger = started(Mischief::default()).await;
        let response = ask(&roger, 1).await.unwrap();

        assert_eq!(response.turn, 1);
        assert_eq!(response.orders.len(), 1);
        assert_eq!(response.orders[0].unit_id, "A1");
    }

    #[tokio::test]
    async fn forgetting_happens_once_so_recovery_can_converge() {
        let roger = started(Mischief {
            forget_on_turn: Some(2),
            ..Mischief::default()
        })
        .await;

        assert!(ask(&roger, 1).await.is_ok());

        let error = ask(&roger, 2).await.unwrap_err();
        assert_eq!(error.code(), Code::FailedPrecondition);
        assert_eq!(error.message(), SHRUBBERY_REQUIRED);

        // The engine's answer to that status: re-send NewMatch, then retry
        // the same turn.
        started_again(&roger).await;
        assert!(ask(&roger, 2).await.is_ok());
    }

    async fn started_again(roger: &Roger) {
        roger
            .new_match(Request::new(NewMatchRequest {
                match_id: "m3-test".to_string(),
                chapter: Chapter::A as i32,
                board: Some(BoardLayout {
                    width: 8,
                    height: 1,
                    shrubbery: vec![],
                }),
                rules: Some(Rules::default()),
                turn: 2,
            }))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn stolen_and_illegal_orders_are_still_well_formed_messages() {
        let thief = started(Mischief {
            steal_knights: true,
            ..Mischief::default()
        })
        .await;
        assert_eq!(ask(&thief, 1).await.unwrap().orders[0].unit_id, "B1");

        let fool = started(Mischief {
            illegal_orders: true,
            ..Mischief::default()
        })
        .await;
        let order = ask(&fool, 1).await.unwrap().orders.remove(0);
        assert_eq!(order.unit_id, "A1");
        assert_eq!(order.move_to, Some(Position { x: 999, y: 999 }));
    }

    #[tokio::test]
    async fn the_wrong_turn_flag_breaks_the_echo() {
        let roger = started(Mischief {
            wrong_turn: true,
            ..Mischief::default()
        })
        .await;

        assert_eq!(ask(&roger, 5).await.unwrap().turn, 6);
    }
}
```

Testing the misbehaving bot may feel absurd — but a broken test fixture makes
the engine look broken, and you will believe the engine. These tests are cheap
insurance: they call the service methods directly, with no server and no
socket.

### 8.6 Run the checkpoint

```sh
cargo fmt
cargo test -p roger-the-shrubber
cargo build --workspace
```

Expected: 4 passed, and `target/debug/roger-the-shrubber` exists.


---

## Checkpoint 9 — the failure-mode integration tests

Unit tests prove the policy is right about statuses it was handed. Only real
processes prove tonic hands over the statuses you think it does.

### 9.1 Create `crates/ni-engine/tests/failure_modes.rs`

```rust
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
```

**How the test finds the binaries.** A file in `tests/` is its own crate; it
can call `ni-engine`'s public API but it cannot depend on the `ni-bot` or
`roger-the-shrubber` *binaries* through Cargo. So it navigates: the test
executable lives in `target/debug/deps/`, and the workspace binaries are one
directory up. The `assert!(path.exists(), ...)` turns "connection refused"
into "run `cargo build --workspace` first", which is the actual mistake.

**What each test is really asserting.** Not the game — the *engine's
composure*. `conclusion.expect("the engine survives a hostile bot")` fails the
test if `run_match` returned an error at all. Every one of these bots is doing
something the engine has no obligation to tolerate, and the engine still has
to produce a result.

Note the pairs. `a_permanently_slow_bot_runs_out_of_strikes` and
`one_slow_turn_costs_a_turn_not_the_match` use the same flag with different
scope, and together they pin the *reset* behaviour — the property most likely
to be broken by a later refactor and least likely to be noticed.

### 9.2 Run the checkpoint

```sh
cargo fmt
cargo build --workspace
cargo test --workspace
```

Expected: everything green, including 7 tests in `failure_modes`. The whole
integration suite runs in well under a second: no test sleeps longer than one
deadline, because the deadlines in the tests are 100–500ms on purpose.


---

## Checkpoint 10 — watch each failure happen

Tests assert. This checkpoint is for looking. Run each command and read the
output; these transcripts are the real thing, trimmed of board frames.

### 10.1 A bot that is always too slow

```sh
./target/debug/ni-engine run \
  --bot-a target/debug/ni-bot \
  --bot-b target/debug/roger-the-shrubber \
  --bot-b-arg --sleep-ms --bot-b-arg 400 \
  --turn-deadline 100ms --strike-limit 3
```

```text
turn 2: GetOrders -> chapter B failed with Cancelled in 100ms
turn 2: chapter B forfeits the turn — missed deadline (no answer within 100ms (Cancelled: Timeout expired)); strike 1/3
turn 3: GetOrders -> chapter A answered in 1ms
turn 3: chapter A acted
...
result: chapter A wins — chapter B forfeits (missed deadline)
note: chapter B forfeited (Timeout)
```

Two things to note. The call fails at *exactly* the deadline — 100ms, not
400ms — so the engine's clock is the one that matters. And chapter A keeps
playing between B's forfeits: strikes are per chapter, and A's turns do not
clear B's counter.

### 10.2 A bot that is slow once

```sh
./target/debug/ni-engine run \
  --bot-a target/debug/ni-bot \
  --bot-b target/debug/roger-the-shrubber \
  --bot-b-arg --sleep-ms --bot-b-arg 400 \
  --bot-b-arg --sleep-on-turn --bot-b-arg 2 \
  --turn-deadline 100ms
```

One `strike 1/3`, then the match plays to a normal conclusion. That is the
counter resetting, and it is the reason a hiccup does not end a game.

### 10.3 A bot that dies mid-call

```sh
./target/debug/ni-engine run \
  --bot-a target/debug/ni-bot \
  --bot-b target/debug/roger-the-shrubber \
  --bot-b-arg --crash-on-turn --bot-b-arg 6
```

```text
roger: dying on turn 6
turn 6: GetOrders -> chapter B failed with Unknown in 1ms
turn 6: chapter B cannot continue (Unknown: transport error (process exited with code 101))
could not notify bot B that the match ended: status: Unavailable, message: "tcp connect error", ...
result: chapter A wins — chapter B forfeits (unreachable)
```

Three different views of one death: `Unknown: transport error` for the call
that was in flight, `code 101` from `exit_status`, and `Unavailable: tcp
connect error` when `MatchEnded` tries to reach a process that is no longer
there. The engine reports the result anyway — that last line is a warning on
stderr, not a failure.

### 10.4 A bot that forgets the match

```sh
./target/debug/ni-engine run \
  --bot-a target/debug/ni-bot \
  --bot-b target/debug/roger-the-shrubber \
  --bot-b-arg --forget-on-turn --bot-b-arg 6
```

```text
roger: forgetting every match at turn 6
turn 6: GetOrders -> chapter B failed with FailedPrecondition in 0ms
turn 6: chapter B has not been brought a shrubbery; re-sending NewMatch and retrying
roger: NewMatch m3-demo as B at turn 6
turn 6: GetOrders -> chapter B answered in 0ms
turn 6: chapter B acted
turn 7: GetOrders -> chapter A answered in 1ms
```

This is the recovery the error name promises, in five lines: forgotten,
demanded, re-sent, retried, continued. No strike is recorded, because nothing
went wrong with the *game* — the bot asked for something it was entitled to
ask for, and got it. This works only because the engine holds all the truth;
a bot that owned any part of the match state could not be restored this way.

### 10.5 A bot that answers about the wrong turn

```sh
./target/debug/ni-engine run \
  --bot-a target/debug/ni-bot \
  --bot-b target/debug/roger-the-shrubber \
  --bot-b-arg --wrong-turn --strike-limit 3
```

```text
turn 2: chapter B forfeits the turn — broken contract (echoed turn 3 while the engine plays 2); strike 1/3
turn 4: chapter B forfeits the turn — broken contract (echoed turn 5 while the engine plays 4); strike 2/3
turn 6: chapter B forfeits the turn — broken contract (echoed turn 7 while the engine plays 6); strike 3/3
result: chapter A wins — chapter B forfeits (broken contract)
```

The echo the engine added for *retry safety* turns out to also be a liveness
check. That is the argument for echoing an operation id in general: it costs
four bytes and it catches a whole class of confusion.

### 10.6 Bots that return nonsense

```sh
./target/debug/ni-engine run \
  --bot-a target/debug/ni-bot \
  --bot-b target/debug/roger-the-shrubber \
  --bot-b-arg --illegal-orders

./target/debug/ni-engine run \
  --bot-a target/debug/ni-bot \
  --bot-b target/debug/roger-the-shrubber \
  --bot-b-arg --steal-knights
```

```text
order B1: illegal, DestinationOob
...
order A1: illegal, NotYourKnight
```

No status codes, no forfeits, no strikes — these never reach `policy.rs` at
all. The bot answered in time and the response parsed; the orders were simply
against the rules, and `ni-game` has said so since M1. Chapter B never acts
legally, so chapter A wins on the board. This is the boundary M3 is drawing:
*transport* failures are the engine's problem, *rules* failures were already
solved, and neither is a crash.


---

## What tonic actually returns

The implementation plan said to observe this rather than assume it. Observed,
with tonic 0.13 over loopback TCP:

| Situation | Status the engine receives | `classify` |
|---|---|---|
| `set_timeout` deadline elapses | `Cancelled: Timeout expired` | `Timeout` |
| Bot process exits during a call | `Unknown: transport error` | `Unreachable` |
| Call to an already-dead process | `Unavailable: tcp connect error` | `Unreachable` |
| Bot returns `FAILED_PRECONDITION` / `SHRUBBERY_REQUIRED` | as sent | `NeedsShrubbery` |
| Bot returns any other status | as sent | `Protocol` |

**The first row is the surprise.** The gRPC specification's status for a
missed deadline is `DEADLINE_EXCEEDED`, and that is what a *server* returns
when it enforces `grpc-timeout` itself. What you get in the client is
`CANCELLED`, because the client-side deadline fired first and cancelled the
call locally; from the caller's seat the call was cancelled, and the reason
happens to be time.

Three consequences worth writing down:

1. **Match on meaning, not on codes.** If `classify` had listed only
   `DeadlineExceeded`, every timeout would have been filed as a broken
   contract. The behaviour would have looked correct — a forfeit either way —
   with the wrong reason in the log and the wrong `MatchEndReason` on the wire.
2. **Status codes are a two-party observation.** The same event is
   `CANCELLED` here and, if the bot's server enforced the deadline first,
   `DEADLINE_EXCEEDED` there. Neither is wrong.
3. **You cannot tell, from `CANCELLED`, whether the bot decided.** It may have
   produced orders a microsecond after the engine stopped listening. That is
   the ambiguity blog post 4 is about, and it is why M3 forfeits the turn
   instead of retrying it.

## Follow one slow turn through your code

A bot sleeps 400ms with a 100ms deadline, on turn 6, for the third time:

1. `run_match` picks `bot_b` and `strikes_b` for chapter B.
2. `request_orders` → `call_get_orders` builds `GetOrdersRequest { turn: 6 }`,
   calls `set_timeout(100ms)`, starts an `Instant`.
3. tonic writes `grpc-timeout: 100m` and sends the request. Roger's handler
   sleeps.
4. At 100ms the client cancels; the call returns
   `Status { code: Cancelled, message: "Timeout expired" }`.
5. `call_get_orders` prints `GetOrders -> chapter B failed with Cancelled in
   100ms` and returns the status.
6. `classify` maps it to `CallFailure::Timeout`.
7. `request_orders` returns `TurnOutcome::TurnForfeited { reason: Timeout }`.
8. `run_match` calls `strikes.record(Timeout)` → count 3.
9. `render_forfeited_turn` prints `strike 3/3`.
10. `strikes.exhausted(3)` returns `Some(Timeout)` → `break
    MatchConclusion::Forfeit { loser: B, reason: Timeout }`.
11. `notify_match_ended` sends `MATCH_END_REASON_FORFEIT_TIMEOUT` to both bots
    (best effort).
12. `render_result` prints `chapter A wins — chapter B forfeits (missed
    deadline)`; `main` exits 0.

No `?` was involved after step 4. That is the design.

## Who owns what

| Concern | Owner | Where |
|---|---|---|
| `Duration`, `Instant`, enums, ownership | Rust | everywhere |
| Timers, child processes, `timeout` | Tokio | `process.rs`, `call_get_orders` |
| `grpc-timeout` on the wire, status codes | tonic / gRPC | `set_timeout`, `Status` |
| What a status *means* for a match | Ni | `policy.rs` |
| Whether a turn or a match is lost | Ni | `policy.rs` + `match_runner.rs` |
| Whether an order is legal | Ni (M1) | `ni-game`, untouched by M3 |

The middle two rows are the milestone. gRPC gives you a vocabulary of
failures; it has no opinion about what your system should do with them, and
M3 is the file where you write that opinion down.


---

## Beginner troubleshooting

### `error: unexpected argument '--sleep-ms' found`

You passed a flag as a value without `allow_hyphen_values = true` on
`--bot-a-arg` / `--bot-b-arg`. Either add the attribute (Checkpoint 6) or use
the `=` form: `--bot-b-arg=--sleep-ms`.

### The integration tests fail with "is missing — run `cargo build --workspace` first"

Exactly what it says. `cargo test` builds test targets and libraries, not the
sibling binaries the tests spawn. Build first.

### The slow bot never forfeits

Your deadline is longer than Roger's sleep. `--sleep-ms 400` needs
`--turn-deadline` below 400ms; the default is 500ms, so nothing happens.

### The strike limit is never reached

Check that `strikes.clear()` is only on the `Orders` arm, and that you kept
*two* counters. A single shared counter is cleared by the opponent's good
turns, and a bot that misbehaves every turn will then never reach the limit.

### `cannot borrow `*bot_a` as mutable more than once`

You wrote `bot_a` instead of `&mut *bot_a` in the actor `match`. Without the
reborrow the first iteration moves the borrow out of the loop.

### `type annotations needed` in `BotProcess::spawn`

The empty slice `&[]` has no element type. Keep the turbofish:
`Self::spawn_with_args::<&OsStr>(path, label, &[])`.

### Recovery loops forever, then forfeits with "demanded a shrubbery again"

Roger's `already_forgot` latch is missing or the `swap` is written as a
`load`. `swap(true, SeqCst)` must return the *previous* value.

### `MatchConclusion` cannot be compared in a test

`assert_eq!` needs `PartialEq`, and the `{conclusion:?}` message needs
`Debug`. Both are on the `#[derive(...)]` line in `policy.rs`.

### The engine prints a timeout as `Protocol`

`classify` is missing `Code::Cancelled`. See "What tonic actually returns".

### Bot processes survive a `Ctrl-C`

`kill_on_drop(true)` covers a dropped `BotProcess`, and `shutdown()` covers
the normal path, but a hard kill of the engine can still orphan a child. Check
with `pgrep -f roger-the-shrubber` and clean up with `pkill`.

## Notes to collect for blog posts 3 and 4

Write these down while the work is fresh; they are the parts you will not
remember later.

- The exact status a `set_timeout` deadline produces on the client, and why it
  is not the one the spec names.
- The difference between a client stopwatch and a propagated deadline, in
  terms of what the *bot* can do with the knowledge.
- The three-line diff between "abort the match on a bad turn echo" and
  "forfeit the turn": a design decision that is almost invisible in code.
- Why the engine retries `SHRUBBERY_REQUIRED` but refuses to retry a timeout,
  stated as a question about what the engine can *know*.
- The forfeit taxonomy — timeout, crash, protocol — and the argument that
  three reasons is enough.
- The observation that a turn number added for idempotency also caught a
  liveness bug.
- Anything Roger did that you did not predict. That list is the post.

## M3 completion checklist

```sh
cargo fmt --check
cargo clippy --workspace --all-targets
cargo build --workspace
cargo test --workspace
```

- [ ] `policy.rs` exists and every decision in M3 goes through it
- [ ] Every gRPC call in the engine has a deadline
- [ ] A missed deadline forfeits the turn and records a strike
- [ ] One good turn clears that chapter's strikes
- [ ] The strike limit forfeits the match, with the right reason
- [ ] `SHRUBBERY_REQUIRED` triggers `NewMatch` + one retry, and the match goes on
- [ ] A crashed bot ends the match with a result, not an error
- [ ] A wrong turn echo costs a turn instead of aborting the match
- [ ] Illegal and stolen orders still resolve through `ni-game`
- [ ] `roger-the-shrubber` builds, and each flag reaches its branch
- [ ] `cargo test --workspace` includes 7 passing failure-mode tests
- [ ] `ni-game` has no new code in it
- [ ] No bot process outlives the engine

M4 next: `tracing` spans around `GetOrders`, W3C trace context in the request
metadata, and a JSONL match log with the latencies this milestone is already
measuring.
