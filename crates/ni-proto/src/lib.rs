//! Generated gRPC types and stubs for the `ni.v1` contract.
//!
//! The contract itself lives in `proto/ni/v1/ni.proto` at the repo root;
//! this crate only exposes what `tonic-build` generates from it. The engine
//! uses `bot_service_client::BotServiceClient`, bots implement
//! `bot_service_server::BotService`.

pub mod ni {
    pub mod v1 {
        tonic::include_proto!("ni.v1");
    }
}

/// gRPC status message accompanying `FAILED_PRECONDITION` when a bot is
/// asked about a match it holds no state for: *you have not brought me a
/// shrubbery*. The engine answers by re-sending `NewMatch`.
pub const SHRUBBERY_REQUIRED: &str = "SHRUBBERY_REQUIRED";

/// Protocol version spoken by this crate's generated code.
pub const PROTOCOL_VERSION: u32 = 1;
