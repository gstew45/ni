//! The match log: one JSON object per line, and the replay it accumulates.
//!
//! Two consumers, one source of truth:
//!
//! - a **JSONL file** you can `tail -f`, `jq`, or paste a trace id out of;
//! - a **`ni.v1.Replay`** message, sent to both bots at the end of the match.
//!
//! Nothing in here may fail a match. A log line that cannot be written is a
//! warning and a dropped writer, never a `?` that unwinds the game.

use std::{
    fs::File,
    io::{BufWriter, Write},
    path::Path,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use ni_game::{Chapter, MatchState, OrderOutcome, OrderResult};
use ni_proto::ni::v1::{
    BoardLayout, KnightOrder, OrderOutcome as ProtoOutcome, OrderResult as ProtoResult, Position,
    Replay, Rules as ProtoRules, TurnRecord,
};
use serde::Serialize;

use crate::{
    convert::{board_layout, chapter_to_proto, rules_to_proto},
    policy::{MatchConclusion, TimeControl},
};

/// What the engine observed about one `GetOrders` call, in the terms the log
/// cares about: how long, how it ended, what it changed.
pub struct TurnEvent<'a> {
    pub turn: u32,
    pub acting: Chapter,
    pub latency: Duration,
    /// `ok`, `timeout`, `unreachable` or `protocol`.
    pub status: &'static str,
    pub detail: Option<&'a str>,
    pub outcomes: &'a [OrderOutcome],
    pub strikes: u32,
    /// Number of `GetOrders` calls this turn: 2 after a `SHRUBBERY_REQUIRED`
    /// recovery, 1 otherwise.
    pub attempts: u32,
}

pub struct MatchLog {
    match_id: String,
    board: BoardLayout,
    rules: ProtoRules,
    writer: Option<BufWriter<File>>,
    turns: Vec<TurnRecord>,
}

impl MatchLog {
    /// `path: None` keeps the replay and skips the file — the engine still
    /// records everything, it just has nowhere to put it.
    pub fn create(
        match_id: &str,
        state: &MatchState,
        time: TimeControl,
        path: Option<&Path>,
    ) -> Result<Self> {
        let mut writer = match path {
            Some(path) => Some(BufWriter::new(File::create(path).with_context(|| {
                format!("could not create match log {}", path.display())
            })?)),
            None => None,
        };

        let match_id = match_id.to_string();
        let board = board_layout(state);

        write(
            &mut writer,
            &Entry::MatchStarted {
                ts_ms: now_ms(),
                match_id: &match_id,
                trace: TraceRef::current(),
                turn_deadline_ms: time.turn_deadline_ms(),
                strike_limit: time.strike_limit,
                board_width: board.width,
                board_height: board.height,
            },
        );

        Ok(Self {
            match_id,
            board,
            rules: rules_to_proto(state, time),
            writer,
            turns: Vec::new(),
        })
    }

    pub fn record_turn(&mut self, event: TurnEvent<'_>) {
        // Destructuring `self` gives disjoint borrows of the fields, so the
        // entry can borrow `match_id` while `writer` is borrowed mutably.
        let Self {
            match_id,
            writer,
            turns,
            ..
        } = self;

        write(
            writer,
            &Entry::Turn {
                ts_ms: now_ms(),
                match_id,
                trace: TraceRef::current(),
                turn: event.turn,
                acting: chapter_name(event.acting),
                status: event.status,
                detail: event.detail,
                latency_us: event.latency.as_micros(),
                deadline_exceeded: event.status == "timeout",
                attempts: event.attempts,
                strikes: event.strikes,
                orders: event.outcomes.iter().map(OrderEntry::from).collect(),
            },
        );

        turns.push(TurnRecord {
            turn: event.turn,
            acting: chapter_to_proto(event.acting),
            outcomes: event.outcomes.iter().map(proto_outcome).collect(),
            get_orders_latency_us: u64::try_from(event.latency.as_micros()).unwrap_or(u64::MAX),
            deadline_exceeded: event.status == "timeout",
        });
    }

    pub fn record_result(&mut self, conclusion: MatchConclusion) {
        let Self {
            match_id,
            writer,
            turns,
            ..
        } = self;

        write(
            writer,
            &Entry::MatchEnded {
                ts_ms: now_ms(),
                match_id,
                trace: TraceRef::current(),
                turns: turns.len(),
                winner: conclusion.winner().map(chapter_name),
                reason: conclusion.end_reason(),
            },
        );

        if let Some(writer) = self.writer.as_mut() {
            if let Err(error) = writer.flush() {
                tracing::warn!(%error, "could not flush the match log");
            }
        }
    }

    /// The replay message, built from the same records the JSONL file got.
    pub fn replay(&self, conclusion: MatchConclusion) -> Replay {
        Replay {
            match_id: self.match_id.clone(),
            board: Some(self.board.clone()),
            rules: Some(self.rules),
            turns: self.turns.clone(),
            winner: conclusion
                .winner()
                .map(chapter_to_proto)
                .unwrap_or_default(),
            reason: conclusion.end_reason(),
        }
    }
}

/// A free function rather than a method, so callers can hold a borrow of
/// another field of `MatchLog` while writing.
fn write(writer: &mut Option<BufWriter<File>>, entry: &Entry<'_>) {
    let Some(sink) = writer.as_mut() else {
        return;
    };

    let line = match serde_json::to_string(entry) {
        Ok(line) => line,
        Err(error) => {
            tracing::warn!(%error, "could not serialise a match log entry");
            return;
        }
    };

    // Flushed per line on purpose: `tail -f` during a match is the point.
    if let Err(error) = writeln!(sink, "{line}").and_then(|()| sink.flush()) {
        tracing::warn!(%error, "match log disabled after a write error");
        *writer = None;
    }
}

/// The three line shapes. `tag = "kind"` puts a `"kind"` field in the JSON,
/// so one file can hold all three and `jq 'select(.kind == "turn")'` works.
#[derive(Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum Entry<'a> {
    MatchStarted {
        ts_ms: u128,
        match_id: &'a str,
        #[serde(flatten)]
        trace: TraceRef,
        turn_deadline_ms: u32,
        strike_limit: u32,
        board_width: u32,
        board_height: u32,
    },
    Turn {
        ts_ms: u128,
        match_id: &'a str,
        #[serde(flatten)]
        trace: TraceRef,
        turn: u32,
        acting: &'static str,
        status: &'static str,
        #[serde(skip_serializing_if = "Option::is_none")]
        detail: Option<&'a str>,
        latency_us: u128,
        deadline_exceeded: bool,
        attempts: u32,
        strikes: u32,
        orders: Vec<OrderEntry>,
    },
    MatchEnded {
        ts_ms: u128,
        match_id: &'a str,
        #[serde(flatten)]
        trace: TraceRef,
        turns: usize,
        #[serde(skip_serializing_if = "Option::is_none")]
        winner: Option<&'static str>,
        reason: i32,
    },
}

/// The bridge between the two halves of M4: every log line carries the ids
/// of the span it happened in, so a line in the file and a span in the
/// dashboard are the same event seen twice.
#[derive(Default, Serialize)]
struct TraceRef {
    #[serde(skip_serializing_if = "Option::is_none")]
    trace_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    span_id: Option<String>,
}

impl TraceRef {
    fn current() -> Self {
        match ni_telemetry::current_ids() {
            Some(ids) => Self {
                trace_id: Some(ids.trace_id),
                span_id: Some(ids.span_id),
            },
            None => Self::default(),
        }
    }
}

#[derive(Serialize)]
struct OrderEntry {
    unit_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    move_to: Option<[u32; 2]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    attack_target: Option<String>,
    result: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    damage: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

impl From<&OrderOutcome> for OrderEntry {
    fn from(outcome: &OrderOutcome) -> Self {
        let (result, damage, reason) = match &outcome.result {
            OrderResult::Applied { damage_dealt } => ("applied", Some(*damage_dealt), None),
            OrderResult::Illegal { reason } => ("illegal", None, Some(format!("{reason:?}"))),
            OrderResult::NotReached => ("not_reached", None, None),
        };

        Self {
            unit_id: outcome.order.unit_id.clone(),
            move_to: outcome
                .order
                .move_to
                .map(|position| [position.x, position.y]),
            attack_target: outcome.order.attack_target.clone(),
            result,
            damage,
            reason,
        }
    }
}

fn proto_outcome(outcome: &OrderOutcome) -> ProtoOutcome {
    let (result, detail, damage_dealt) = match &outcome.result {
        OrderResult::Applied { damage_dealt } => {
            (ProtoResult::Applied, String::new(), *damage_dealt)
        }
        OrderResult::Illegal { reason } => (ProtoResult::Illegal, format!("{reason:?}"), 0),
        OrderResult::NotReached => (ProtoResult::NotReached, String::new(), 0),
    };

    ProtoOutcome {
        order: Some(KnightOrder {
            unit_id: outcome.order.unit_id.clone(),
            move_to: outcome.order.move_to.map(|position| Position {
                x: position.x,
                y: position.y,
            }),
            attack_target: outcome.order.attack_target.clone(),
        }),
        result: result as i32,
        detail,
        damage_dealt,
    }
}

fn chapter_name(chapter: Chapter) -> &'static str {
    match chapter {
        Chapter::A => "A",
        Chapter::B => "B",
    }
}

fn now_ms() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|since| since.as_millis())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ni_game::{Order, Position as GamePosition, Rules};

    fn applied(unit: &str, damage: u32) -> OrderOutcome {
        OrderOutcome {
            order: Order {
                unit_id: unit.to_string(),
                move_to: Some(GamePosition { x: 3, y: 4 }),
                attack_target: Some("B1".to_string()),
            },
            result: OrderResult::Applied {
                damage_dealt: damage,
            },
        }
    }

    fn log_in(directory: &Path) -> (MatchLog, std::path::PathBuf) {
        let path = directory.join("match.jsonl");
        let state = ni_game::standard_match(Rules::standard());
        let log =
            MatchLog::create("m4-test", &state, TimeControl::standard(), Some(&path)).unwrap();
        (log, path)
    }

    #[test]
    fn every_turn_is_one_line_of_json() {
        let directory = std::env::temp_dir().join("ni-m4-log-lines");
        std::fs::create_dir_all(&directory).unwrap();
        let (mut log, path) = log_in(&directory);

        log.record_turn(TurnEvent {
            turn: 1,
            acting: Chapter::A,
            latency: Duration::from_micros(1500),
            status: "ok",
            detail: None,
            outcomes: &[applied("A1", 4)],
            strikes: 0,
            attempts: 1,
        });
        log.record_turn(TurnEvent {
            turn: 2,
            acting: Chapter::B,
            latency: Duration::from_millis(100),
            status: "timeout",
            detail: Some("no answer within 100ms"),
            outcomes: &[],
            strikes: 1,
            attempts: 1,
        });
        log.record_result(MatchConclusion::Forfeit {
            loser: Chapter::B,
            reason: crate::policy::ForfeitReason::Timeout,
        });

        let text = std::fs::read_to_string(&path).unwrap();
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 4, "started + two turns + ended: {text}");

        let turn: serde_json::Value = serde_json::from_str(lines[1]).unwrap();
        assert_eq!(turn["kind"], "turn");
        assert_eq!(turn["latency_us"], 1500);
        assert_eq!(turn["orders"][0]["result"], "applied");
        assert_eq!(turn["orders"][0]["damage"], 4);
        assert_eq!(turn["orders"][0]["move_to"], serde_json::json!([3, 4]));

        let forfeit: serde_json::Value = serde_json::from_str(lines[2]).unwrap();
        assert_eq!(forfeit["status"], "timeout");
        assert_eq!(forfeit["deadline_exceeded"], true);
        assert_eq!(forfeit["strikes"], 1);
        assert_eq!(forfeit["orders"], serde_json::json!([]));

        let ended: serde_json::Value = serde_json::from_str(lines[3]).unwrap();
        assert_eq!(ended["kind"], "match_ended");
        assert_eq!(ended["turns"], 2);
        assert_eq!(ended["winner"], "A");
    }

    #[test]
    fn the_replay_carries_the_same_turns_as_the_file() {
        let directory = std::env::temp_dir().join("ni-m4-log-replay");
        std::fs::create_dir_all(&directory).unwrap();
        let (mut log, _) = log_in(&directory);

        log.record_turn(TurnEvent {
            turn: 1,
            acting: Chapter::A,
            latency: Duration::from_micros(900),
            status: "ok",
            detail: None,
            outcomes: &[applied("A1", 4)],
            strikes: 0,
            attempts: 1,
        });

        let replay = log.replay(MatchConclusion::Decided(ni_game::MatchStatus::Winner {
            chapter: Chapter::A,
            reason: ni_game::EndReason::Elimination,
        }));

        assert_eq!(replay.match_id, "m4-test");
        assert_eq!(replay.turns.len(), 1);
        assert_eq!(replay.turns[0].get_orders_latency_us, 900);
        assert_eq!(replay.turns[0].outcomes[0].damage_dealt, 4);
        assert_eq!(replay.board.unwrap().width, 10);
        assert!(replay.rules.unwrap().turn_deadline_ms > 0);
    }

    #[test]
    fn no_path_means_no_file_but_still_a_replay() {
        let state = ni_game::standard_match(Rules::standard());
        let mut log = MatchLog::create("m4-quiet", &state, TimeControl::standard(), None).unwrap();

        log.record_turn(TurnEvent {
            turn: 1,
            acting: Chapter::A,
            latency: Duration::from_micros(10),
            status: "ok",
            detail: None,
            outcomes: &[],
            strikes: 0,
            attempts: 1,
        });

        assert_eq!(
            log.replay(MatchConclusion::Decided(ni_game::MatchStatus::Draw {
                reason: ni_game::EndReason::TurnCap,
            }))
            .turns
            .len(),
            1
        );
    }
}
