//! M2 match orchestration around the pure `ni-game` rules engine.

pub mod convert;
pub mod log;
pub mod match_runner;
pub mod policy;
pub mod process;
pub mod render;
pub mod transport;

pub use log::MatchLog;
pub use match_runner::{run_match, RunOptions};
pub use policy::{ForfeitReason, MatchConclusion, TimeControl};
pub use process::BotProcess;
pub use transport::{Listen, SocketDir, Transport};
