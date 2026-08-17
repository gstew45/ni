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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TimeControl {
    pub turn_deadline: Duration,
    pub strike_limit: u32,
}

impl TimeControl {
    pub fn standard() -> Self {
        Self {
            turn_deadline: Duration::from_millis(500),
            strike_limit: 3,
        }
    }

    pub fn turn_deadline_ms(self) -> u32 {
        u32::try_from(self.turn_deadline.as_millis()).unwrap_or(u32::MAX)
    }
}

impl Default for TimeControl {
    fn default() -> Self {
        Self::standard()
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ForfeitReason {
    Timeout,
    Crash,
    Protocol,
}

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum CallFailure {
    Timeout,
    NeedsShrubbery,
    Unreachable(String),
    Protocol(String),
}

pub fn classify(status: &Status) -> CallFailure {
    match status.code() {
        Code::DeadlineExceeded | Code::Cancelled => CallFailure::Timeout,
        Code::FailedPrecondition if status.message().contains(SHRUBBERY_REQUIRED) => {
            CallFailure::NeedsShrubbery
        }
        Code::Unavailable | Code::Unknown => CallFailure::Unreachable(describe(status)),
        _ => CallFailure::Protocol(describe(status)),
    }
}

pub fn describe(status: &Status) -> String {
    format!("{:?}: {}", status.code(), status.message())
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct Strikes {
    count: u32,
    last: Option<ForfeitReason>,
}

impl Strikes {
    pub fn record(&mut self, reason: ForfeitReason) {
        self.count += 1;
        self.last = Some(reason)
    }

    pub fn clear(&mut self) {
        self.count = 0;
        self.last = None;
    }

    pub fn count(self) -> u32 {
        self.count
    }

    pub fn exhausted(self, limit: u32) -> Option<ForfeitReason> {
        match self.last {
            Some(reason) if self.count >= limit => Some(reason),
            _ => None,
        }
    }
}

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
