//! The Ni rules engine: pure, deterministic, synchronous.
//!
//! Board, knights, line of sight, cover, order validation and resolution
//! all live here, with no gRPC, no async and no I/O, so the rules can be
//! unit-tested in isolation and matches stay byte-for-byte reproducible.
//!
//! This is milestone **M1**. gRPC / tonic / tokio land in **M2**
//! (`ni-engine` + `ni-bot`) — they sit on top of these types:
//!
//! ```text
//!   ni-bot  (gRPC *server*, M2)     ni-engine  (gRPC *client*, M2)
//!        \                              /
//!         \   proto messages on the    /
//!          \  wire (ni.v1.KnightOrder)/
//!           \                        /
//!            +-->  ni-game  <------+
//!                  MatchState, Order, apply_orders
//!                  (this crate — no proto types in sight)
//! ```
//!
//! Keeping this crate proto-free is deliberate. The engine will translate
//! `ni.v1.KnightOrder` → [`Order`] and [`MatchState`] → `BattlefieldView`.
//! That translation *is* the service boundary the blog trace is about.
//!
//! Implementation plan: `implementation-plan.md`, milestone M1.

pub mod apply;
pub mod layout;
pub mod los;
pub mod types;

pub use apply::{apply_orders, end_turn, match_status, EndReason, MatchStatus};
pub use layout::standard_match;
pub use los::has_line_of_sight;
pub use types::{
    Board, Chapter, IllegalReason, Knight, MatchState, Order, OrderOutcome, OrderResult, Position,
    Rules,
};
