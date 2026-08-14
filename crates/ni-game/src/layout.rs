//! Fixed symmetric starting layout.
//!
//! Generated maps would make matches incomparable (bad for golden tests and
//! for the measurement posts later). One shipped layout, mirrored across
//! the vertical midline, is the whole of Q6 from the design doc.

use crate::types::{Board, Chapter, Knight, MatchState, Position, Rules};

/// A 10×10 board, four knights a side, shrubbery clustered near centre
/// and mirrored so neither chapter has a terrain advantage.
///
/// ```text
///      0 1 2 3 4 5 6 7 8 9
///    0 · · · · · · · · · ·
///    1 · A1· · · · · · B1·
///    2 · · · # # # # · · ·
///    3 · A2· · · · · · B2·
///    4 · · · · # # · · · ·
///    5 · · · · # # · · · ·
///    6 · A3· · · · · · B3·
///    7 · · · # # # # · · ·
///    8 · A4· · · · · · B4·
///    9 · · · · · · · · · ·
/// ```
///
/// Chapter A starts on the left (`x = 1`), B on the right (`x = 8`).
/// `to_act` is A, `turn` is 1.
pub fn standard_match(rules: Rules) -> MatchState {
    let shrubbery = [
        Position::new(3, 2),
        Position::new(4, 2),
        Position::new(5, 2),
        Position::new(6, 2),
        Position::new(4, 4),
        Position::new(5, 4),
        Position::new(4, 5),
        Position::new(5, 5),
        Position::new(3, 7),
        Position::new(4, 7),
        Position::new(5, 7),
        Position::new(6, 7),
    ];

    let spawn = |id: &str, chapter: Chapter, pos: Position| Knight {
        id: id.to_string(),
        chapter,
        pos,
        hp: rules.knight_hp,
        move_range: rules.move_range,
        attack_range: rules.attack_range,
        damage: rules.attack_damage,
    };

    MatchState {
        board: Board::new(10, 10, shrubbery),
        knights: vec![
            spawn("A1", Chapter::A, Position::new(1, 1)),
            spawn("A2", Chapter::A, Position::new(1, 3)),
            spawn("A3", Chapter::A, Position::new(1, 6)),
            spawn("A4", Chapter::A, Position::new(1, 8)),
            spawn("B1", Chapter::B, Position::new(8, 1)),
            spawn("B2", Chapter::B, Position::new(8, 3)),
            spawn("B3", Chapter::B, Position::new(8, 6)),
            spawn("B4", Chapter::B, Position::new(8, 8)),
        ],
        turn: 1,
        to_act: Chapter::A,
        rules,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_mirror_symmetric() {
        let state = standard_match(Rules::standard());
        assert_eq!(state.board.width, 10);
        assert_eq!(state.board.height, 10);
        assert_eq!(state.living(Chapter::A).count(), 4);
        assert_eq!(state.living(Chapter::B).count(), 4);

        // Every shrubbery tile (x, y) has a twin (width-1-x, y).
        for pos in &state.board.shrubbery {
            let mirror = Position::new(state.board.width - 1 - pos.x, pos.y);
            assert!(
                state.board.has_shrubbery(mirror),
                "{pos:?} has no mirror {mirror:?}"
            );
        }

        // Every A knight has a B knight mirrored across the same midline.
        for a in state.living(Chapter::A) {
            let want = Position::new(state.board.width - 1 - a.pos.x, a.pos.y);
            assert!(
                state.living(Chapter::B).any(|b| b.pos == want),
                "no B knight at the mirror of {} ({:?})",
                a.id,
                a.pos
            );
        }
    }
}
