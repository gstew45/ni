//! The parts of the measurement harness that have no I/O in them.
//!
//! Percentiles and payload construction are pure functions over numbers and
//! messages, so they are unit-testable — which matters more here than
//! elsewhere. A benchmark that reports the wrong number is worse than no
//! benchmark, and "the p99 was computed correctly" is not something you can
//! see by looking at the output.

use std::time::Duration;

use ni_proto::ni::v1::{
    BoardLayout, Chapter, KnightOrder, OrderOutcome, OrderResult, Position, Replay, Rules,
    TurnRecord,
};
use prost::Message as _;
use serde::Serialize;

/// One transport/payload combination's worth of timings.
#[derive(Debug, Default)]
pub struct Samples {
    latencies: Vec<Duration>,
}

impl Samples {
    pub fn with_capacity(capacity: usize) -> Self {
        Self {
            latencies: Vec::with_capacity(capacity),
        }
    }

    pub fn push(&mut self, latency: Duration) {
        self.latencies.push(latency);
    }

    pub fn len(&self) -> usize {
        self.latencies.len()
    }

    pub fn is_empty(&self) -> bool {
        self.latencies.is_empty()
    }

    /// Nearest-rank percentile: sort, then take the ceil(p × n)-th sample.
    ///
    /// No interpolation. Interpolating between two observed latencies invents
    /// a latency that was never measured, which is a strange thing to do to
    /// data you gathered specifically because you did not want to model it.
    pub fn percentile(&self, percentile: f64) -> Duration {
        if self.latencies.is_empty() {
            return Duration::ZERO;
        }

        let mut sorted = self.latencies.clone();
        sorted.sort_unstable();

        let rank = (percentile / 100.0 * sorted.len() as f64).ceil() as usize;
        let index = rank.saturating_sub(1).min(sorted.len() - 1);
        sorted[index]
    }

    pub fn mean(&self) -> Duration {
        if self.latencies.is_empty() {
            return Duration::ZERO;
        }

        let total: Duration = self.latencies.iter().sum();
        total / self.latencies.len() as u32
    }

    /// How many calls took longer than `threshold`.
    ///
    /// A percentile hides how many samples are in the tail — p99 of 5000 calls
    /// is one number standing in for fifty. This counts them, which is what
    /// you want when the interesting behaviour is rare and large rather than
    /// common and small.
    pub fn slower_than(&self, threshold: Duration) -> usize {
        self.latencies
            .iter()
            .filter(|latency| **latency > threshold)
            .count()
    }

    pub fn summarise(&self, transport: &'static str, case: &str, payload_bytes: usize) -> Summary {
        Summary {
            transport,
            case: case.to_string(),
            payload_bytes,
            calls: self.len(),
            min_us: self.percentile(0.0).as_micros(),
            p50_us: self.percentile(50.0).as_micros(),
            p90_us: self.percentile(90.0).as_micros(),
            p99_us: self.percentile(99.0).as_micros(),
            max_us: self.percentile(100.0).as_micros(),
            mean_us: self.mean().as_micros(),
            over_10ms: self.slower_than(Duration::from_millis(10)),
        }
    }
}

/// One row of the results table.
#[derive(Debug, Serialize)]
pub struct Summary {
    pub transport: &'static str,
    pub case: String,
    /// Encoded size of the *request* message, measured rather than assumed.
    pub payload_bytes: usize,
    pub calls: usize,
    pub min_us: u128,
    pub p50_us: u128,
    pub p90_us: u128,
    pub p99_us: u128,
    pub max_us: u128,
    pub mean_us: u128,
    /// Calls slower than 10ms — two orders of magnitude above the median for
    /// every case except the 1 MiB one, so on the small payloads this counts
    /// events that have an *explanation* rather than a distribution.
    pub over_10ms: usize,
}

impl Summary {
    /// Sequential round-trips per second, derived from the median.
    ///
    /// This is a latency harness, not a throughput harness: one call at a
    /// time, so this number is `1 / p50` and nothing more ambitious.
    pub fn calls_per_second(&self) -> f64 {
        if self.p50_us == 0 {
            return f64::INFINITY;
        }

        1_000_000.0 / self.p50_us as f64
    }

    /// Payload bytes per second at the median latency, in MiB/s.
    pub fn payload_mib_per_second(&self) -> f64 {
        if self.p50_us == 0 {
            return 0.0;
        }

        (self.payload_bytes as f64 * 1_000_000.0) / (self.p50_us as f64 * 1024.0 * 1024.0)
    }
}

/// Grow a replay until it encodes to at least `target_bytes`.
///
/// The point of doing it this way — rather than adding a `bytes padding = 99`
/// field to the proto — is that every byte measured is a byte the real
/// contract would carry. A 1 MiB replay is a plausible tournament artifact,
/// not a bag of zeroes, so protobuf is doing the same varint-and-tag work it
/// would do in production.
pub fn replay_of_at_least(target_bytes: usize) -> Replay {
    let mut replay = Replay {
        match_id: "ni-bench".to_string(),
        board: Some(BoardLayout {
            width: 10,
            height: 10,
            shrubbery: (0..12)
                .map(|index| Position {
                    x: index % 10,
                    y: index / 10,
                })
                .collect(),
        }),
        rules: Some(Rules {
            knight_hp: 10,
            move_range: 2,
            attack_range: 3,
            attack_damage: 3,
            cover_damage_reduction: 1,
            turn_cap: 100,
            turn_deadline_ms: 500,
            timeout_strike_limit: 3,
        }),
        turns: Vec::new(),
        winner: Chapter::A as i32,
        reason: 1,
    };

    // `encoded_len` walks the whole message, so calling it once per pushed
    // turn is quadratic — 70 seconds for a 1 MiB replay in a debug build.
    // Measure one turn, jump most of the way there, then top up.
    let empty = replay.encoded_len();
    replay.turns.push(turn_record(1));
    let per_turn = replay.encoded_len() - empty;

    let estimate = target_bytes.saturating_sub(empty) / per_turn.max(1);
    for turn in 2..=estimate as u32 {
        replay.turns.push(turn_record(turn));
    }

    // The estimate runs slightly short: a turn number past 127 needs a second
    // varint byte, so later records are a byte or two larger than the first.
    while replay.encoded_len() < target_bytes {
        replay
            .turns
            .push(turn_record(replay.turns.len() as u32 + 1));
    }

    replay
}

fn turn_record(turn: u32) -> TurnRecord {
    TurnRecord {
        turn,
        acting: if turn.is_multiple_of(2) {
            Chapter::B as i32
        } else {
            Chapter::A as i32
        },
        outcomes: (1..=4)
            .map(|unit| OrderOutcome {
                order: Some(KnightOrder {
                    unit_id: format!("A{unit}"),
                    move_to: Some(Position {
                        x: (turn + unit) % 10,
                        y: (turn * 3 + unit) % 10,
                    }),
                    attack_target: Some(format!("B{unit}")),
                }),
                result: OrderResult::Applied as i32,
                detail: String::new(),
                damage_dealt: 3,
            })
            .collect(),
        get_orders_latency_us: 420 + u64::from(turn),
        deadline_exceeded: false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn samples(millis: &[u64]) -> Samples {
        let mut samples = Samples::default();
        for value in millis {
            samples.push(Duration::from_millis(*value));
        }
        samples
    }

    #[test]
    fn percentiles_pick_real_observations() {
        // 100 samples, 1ms..100ms: the p-th percentile is the p-th sample.
        let values: Vec<u64> = (1..=100).collect();
        let samples = samples(&values);

        assert_eq!(samples.percentile(50.0), Duration::from_millis(50));
        assert_eq!(samples.percentile(90.0), Duration::from_millis(90));
        assert_eq!(samples.percentile(99.0), Duration::from_millis(99));
        assert_eq!(samples.percentile(100.0), Duration::from_millis(100));
        assert_eq!(samples.percentile(0.0), Duration::from_millis(1));
    }

    #[test]
    fn percentiles_do_not_care_about_arrival_order() {
        let ordered = samples(&[1, 2, 3, 4, 5]);
        let shuffled = samples(&[4, 1, 5, 2, 3]);

        assert_eq!(ordered.percentile(50.0), shuffled.percentile(50.0));
        assert_eq!(ordered.percentile(99.0), shuffled.percentile(99.0));
    }

    #[test]
    fn an_empty_sample_set_reports_zero_instead_of_panicking() {
        let empty = Samples::default();

        assert!(empty.is_empty());
        assert_eq!(empty.percentile(50.0), Duration::ZERO);
        assert_eq!(empty.mean(), Duration::ZERO);
    }

    #[test]
    fn a_single_sample_is_every_percentile() {
        let one = samples(&[7]);

        assert_eq!(one.percentile(0.0), Duration::from_millis(7));
        assert_eq!(one.percentile(50.0), Duration::from_millis(7));
        assert_eq!(one.percentile(100.0), Duration::from_millis(7));
    }

    #[test]
    fn a_replay_grows_to_the_requested_size_and_stays_close_to_it() {
        for target in [1_024, 64 * 1_024, 1_024 * 1_024] {
            let replay = replay_of_at_least(target);
            let encoded = replay.encoded_len();

            assert!(encoded >= target, "{encoded} < {target}");
            // One turn record is ~110 bytes, so overshoot is bounded by one.
            assert!(
                encoded < target + 256,
                "{encoded} overshoots {target} by more than one turn"
            );
        }
    }

    #[test]
    fn outliers_are_counted_not_just_ranked() {
        let mut samples = samples(&[1, 1, 1, 1, 1]);
        samples.push(Duration::from_millis(42));
        samples.push(Duration::from_millis(41));

        assert_eq!(samples.slower_than(Duration::from_millis(10)), 2);
        // Two outliers in seven samples do not reach the 50th percentile...
        assert_eq!(samples.percentile(50.0), Duration::from_millis(1));
        // ...but they are exactly what the top of the distribution is made of.
        assert_eq!(samples.percentile(100.0), Duration::from_millis(42));
    }

    #[test]
    fn a_summary_derives_rates_from_the_median() {
        let mut samples = Samples::default();
        samples.push(Duration::from_micros(1_000));
        let summary = samples.summarise("unix", "SubmitReplay", 1_048_576);

        assert_eq!(summary.p50_us, 1_000);
        assert!((summary.calls_per_second() - 1_000.0).abs() < 0.001);
        assert!((summary.payload_mib_per_second() - 1_000.0).abs() < 1.0);
    }
}
