//! Plain deterministic text rendering of authoritative game state.

use std::fmt::Write as _;

use ni_game::{Chapter, EndReason, MatchState, MatchStatus, OrderOutcome, OrderResult};

use crate::policy::{ForfeitReason, MatchConclusion, Strikes};

pub fn render_board(state: &MatchState) -> String {
    let mut output = String::new();
    output.push_str("   ");

    for x in 0..state.board.width {
        write!(output, " {x:>2}").expect("writing to String cannot fail");
    }
    output.push('\n');

    for y in 0..state.board.height {
        write!(output, "{y:>2} ").expect("writing to String cannot fail");

        for x in 0..state.board.width {
            let position = ni_game::Position { x, y };

            let cell = if let Some(knight) = state.living_at(position) {
                knight.id.as_str()
            } else if state.board.has_shrubbery(position) {
                "#"
            } else {
                "·"
            };

            write!(output, " {cell:>2}").expect("writing to String cannot fail");
        }
        output.push('\n');
    }

    output
}

pub fn render_turn(
    state: &MatchState,
    acting: Chapter,
    turn: u32,
    outcomes: &[OrderOutcome],
) -> String {
    let mut output = format!("turn {turn}: chapter {} acted\n", chapter_name(acting));

    output.push_str(&render_board(state));
    output.push_str("hp:");

    for knight in state.knights.iter().filter(|knight| knight.is_alive()) {
        write!(output, " {}={}", knight.id, knight.hp).expect("writing to String cannot fail");
    }
    output.push('\n');

    if outcomes.is_empty() {
        output.push_str("orders: pass\n");
    } else {
        for outcome in outcomes {
            write!(output, "order {}: ", outcome.order.unit_id)
                .expect("writing to String cannot fail");

            match &outcome.result {
                OrderResult::Applied { damage_dealt } => {
                    writeln!(output, "applied, damage={damage_dealt}")
                        .expect("writing to String cannot fail");
                }
                OrderResult::Illegal { reason } => {
                    writeln!(output, "illegal, {reason:?}").expect("writing to String cannot fail");
                }
                OrderResult::NotReached => {
                    output.push_str("not reached\n");
                }
            }
        }
    }

    output
}

pub fn render_forfeited_turn(
    turn: u32,
    acting: Chapter,
    reason: ForfeitReason,
    detail: &str,
    strikes: Strikes,
    limit: u32,
) -> String {
    format!(
        "turn {turn}: chapter {} forfeits the turn - {} ({detail}); strike {}/{limit}\n",
        chapter_name(acting),
        forfeit_name(reason),
        strikes.count(),
    )
}

pub fn render_result(conclusion: MatchConclusion) -> String {
    match conclusion {
        MatchConclusion::Decided(MatchStatus::InProgress) => {
            "result: match strill in progress".to_string()
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
            "result: chapter {} wins - chapter {} forfeits ({})",
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

fn chapter_name(chapter: Chapter) -> &'static str {
    match chapter {
        Chapter::A => "A",
        Chapter::B => "B",
    }
}

fn reason_name(reason: EndReason) -> &'static str {
    match reason {
        EndReason::Elimination => "elimination",
        EndReason::TurnCap => "turn cap",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ni_game::Rules;

    #[test]
    fn board_renders_knights_and_shrubbery() {
        let state = ni_game::standard_match(Rules::standard());
        let board = render_board(&state);

        assert!(board.contains("A1"));
        assert!(board.contains('#'));
    }

    #[test]
    fn living_knight_wins_over_shrubbery_and_dead_knight_disappears() {
        let mut state = ni_game::standard_match(Rules::standard());
        let a1_position = state.knight("A1").unwrap().pos;

        state.board.shrubbery.clear();
        state.board.shrubbery.insert(a1_position);

        assert!(render_board(&state).contains("A1"));
        assert!(!render_board(&state).contains('#'));

        state
            .knights
            .iter_mut()
            .find(|knight| knight.id == "A1")
            .unwrap()
            .hp = 0;

        let board = render_board(&state);
        assert!(!board.contains("A1"));
        assert!(board.contains('#'));
    }

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
}
