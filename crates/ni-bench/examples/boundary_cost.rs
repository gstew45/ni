//! How much of a round trip is protobuf, with no wire involved at all?
//!
//! `ni-bench` measures the whole boundary. This measures the part of it that
//! no choice of socket can remove: encoding on one side and decoding on the
//! other. Subtract these from the harness's numbers and what is left is
//! everything the transport, HTTP/2 and the scheduler contribute together.
//!
//! Run it with `cargo run --release -p ni-bench --example boundary_cost`.
use std::time::Instant;

use ni_bench::replay_of_at_least;
use ni_proto::ni::v1::SubmitReplayRequest;
use prost::Message as _;

fn main() {
    for target in [1024usize, 1024 * 10, 1024 * 128, 1024 * 512, 1024 * 1024] {
        let message = SubmitReplayRequest {
            replay: Some(replay_of_at_least(target)),
        };
        let bytes = message.encode_to_vec();

        // Best-of, not mean: this is a floor measurement, and the fastest
        // observed run is the one least polluted by everything else on the box.
        let mut encode = std::time::Duration::MAX;
        let mut decode = std::time::Duration::MAX;
        let mut clone = std::time::Duration::MAX;

        for _ in 0..200 {
            let started = Instant::now();
            let buffer = message.encode_to_vec();
            encode = encode.min(started.elapsed());
            std::hint::black_box(buffer);

            let started = Instant::now();
            let decoded = SubmitReplayRequest::decode(bytes.as_slice()).unwrap();
            decode = decode.min(started.elapsed());
            std::hint::black_box(decoded);

            // Not part of the boundary — but `ni-bench` clones this message
            // once per iteration, so it is worth knowing it is cheaper than
            // the encode it feeds. (It is measured outside the timed region
            // there; this is how we know that was worth doing.)
            let started = Instant::now();
            let copied = message.clone();
            clone = clone.min(started.elapsed());
            std::hint::black_box(copied);
        }

        println!(
            "{:>9} bytes  encode {:>9?}  decode {:>9?}  clone {:>9?}",
            bytes.len(),
            encode,
            decode,
            clone
        );
    }
}
