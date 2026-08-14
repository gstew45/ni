//! The Ni rules engine: pure, deterministic, synchronous.
//!
//! Board, knights, line of sight, cover, order validation and resolution
//! all live here, with no gRPC, no async and no I/O, so the rules can be
//! unit-tested in isolation and matches stay byte-for-byte reproducible.
//!
//! Implementation lands in milestone M1 — see `implementation-plan.md`.
