//! Line of sight and cover.
//!
//! Shrubbery does two jobs (design doc):
//! - **Blocks** attacks whose line *crosses* a shrubbery tile.
//! - **Cover** for a unit *standing in* shrubbery: the attack lands, but
//!   damage is reduced by [`crate::Rules::cover_damage_reduction`].
//!
//! Attacker-in-shrubbery shoots out at full damage. Destination shrubbery
//! is cover, not a block — otherwise you could never hit a hidden knight.

use crate::types::{Board, Position, Rules};

/// True iff no *intermediate* shrubbery tile sits on the line from `from`
/// to `to`.
///
/// Endpoints are ignored: shrubbery you stand in does not block your own
/// shot, and shrubbery the target stands in is cover (see
/// [`damage_after_cover`]), not a wall.
///
/// Adjacent tiles always have LOS — there is no tile between them.
pub fn has_line_of_sight(board: &Board, from: Position, to: Position) -> bool {
    tiles_on_line(from, to)
        .into_iter()
        .filter(|p| *p != from && *p != to)
        .all(|p| !board.has_shrubbery(p))
}

/// Every tile the shot crosses, walking from the *center* of `from` to the
/// *center* of `to`.
///
/// This is a 2D DDA (Amanatides & Woo), not textbook Bresenham. Bresenham
/// picks one tile per diagonal step and is **not symmetric**: A→B and B→A
/// can visit different tiles, so one knight could shoot the other but not
/// vice versa. Walking from cell centers with an integer decision keeps
/// the *set* of tiles the same in both directions.
///
/// The decision at each step is "which grid line do we hit next?":
///
/// ```text
/// nx, ny = how many vertical / horizontal grid lines we must cross
/// ix, iy = how many we have already crossed
///
/// compare (2*ix + 1) * ny    vs    (2*iy + 1) * nx
///          ^ next vertical              ^ next horizontal
///
/// Less    → the shot hits a vertical   line first → step x
/// Greater → the shot hits a horizontal line first → step y
/// Equal   → the shot goes through a grid vertex   → step diagonally
/// ```
///
/// `(2*ix + 1)` is the integer form of `ix + 0.5` — "the next crossing is
/// halfway through the current cell, because we started at its center."
/// Cross-multiplying drops the division so we never touch floats.
///
/// On a perfect diagonal, `Equal` fires every step, so a hedge sitting on
/// the *other* corner of a vertex does not block. That is deliberate and
/// tested.
fn tiles_on_line(from: Position, to: Position) -> Vec<Position> {
    let mut tiles = vec![from];
    if from == to {
        return tiles;
    }

    // Board is 10×10; i32 keeps sign and subtraction obvious.
    let mut x = from.x as i32;
    let mut y = from.y as i32;
    let x1 = to.x as i32;
    let y1 = to.y as i32;

    let nx = (x1 - x).abs();
    let ny = (y1 - y).abs();
    let sx = (x1 - x).signum();
    let sy = (y1 - y).signum();

    let mut ix = 0;
    let mut iy = 0;
    while ix < nx || iy < ny {
        match ((2 * ix + 1) * ny).cmp(&((2 * iy + 1) * nx)) {
            std::cmp::Ordering::Less => {
                x += sx;
                ix += 1;
            }
            std::cmp::Ordering::Greater => {
                y += sy;
                iy += 1;
            }
            std::cmp::Ordering::Equal => {
                x += sx;
                y += sy;
                ix += 1;
                iy += 1;
            }
        }
        tiles.push(Position::new(x as u32, y as u32));
    }
    debug_assert_eq!(tiles.last().copied(), Some(to));
    tiles
}

/// Damage actually dealt, after cover.
///
/// ```text
/// if board.has_shrubbery(target_pos):
///     rules.attack_damage.saturating_sub(rules.cover_damage_reduction)
/// else:
///     rules.attack_damage
/// ```
///
/// `saturating_sub` so cover ≥ damage yields 0, never a wrapping underflow.
/// A 0-damage hit is still [`crate::OrderResult::Applied`].
pub fn damage_after_cover(rules: &Rules, board: &Board, target_pos: Position) -> u32 {
    if board.has_shrubbery(target_pos) {
        rules
            .attack_damage
            .saturating_sub(rules.cover_damage_reduction)
    } else {
        rules.attack_damage
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hedge_between() -> Board {
        // A single shrubbery tile at (1, 0), between (0,0) and (2,0).
        Board::new(5, 5, [Position::new(1, 0)])
    }

    #[test]
    fn adjacent_always_visible() {
        let board = hedge_between();
        assert!(has_line_of_sight(
            &board,
            Position::new(0, 0),
            Position::new(0, 1)
        ));
    }

    #[test]
    fn intermediate_shrubbery_blocks() {
        let board = hedge_between();
        assert!(!has_line_of_sight(
            &board,
            Position::new(0, 0),
            Position::new(2, 0)
        ));
    }

    #[test]
    fn target_standing_in_shrubbery_is_cover_not_a_block() {
        let board = hedge_between();
        // Shooter at (0,0), target in the hedge at (1,0): the hedge is the
        // *endpoint*, so LOS is clear and damage_after_cover will reduce.
        assert!(has_line_of_sight(
            &board,
            Position::new(0, 0),
            Position::new(1, 0)
        ));
        let rules = Rules::standard();
        assert_eq!(
            damage_after_cover(&rules, &board, Position::new(1, 0)),
            rules.attack_damage - rules.cover_damage_reduction
        );
    }

    #[test]
    fn attacker_in_shrubbery_shoots_out_at_full_damage() {
        let board = hedge_between();
        let rules = Rules::standard();
        assert_eq!(
            damage_after_cover(&rules, &board, Position::new(2, 0)),
            rules.attack_damage
        );
        assert!(has_line_of_sight(
            &board,
            Position::new(1, 0),
            Position::new(3, 0)
        ));
    }

    #[test]
    fn empty_board_has_los_everywhere() {
        let board = Board::new(10, 10, []);
        assert!(has_line_of_sight(
            &board,
            Position::new(0, 0),
            Position::new(9, 9)
        ));
    }

    #[test]
    fn los_is_symmetric() {
        // Classic Bresenham visits (1,0) one way and (1,1) the other.
        // A hedge on either tile must block both directions.
        let a = Position::new(0, 0);
        let b = Position::new(2, 1);
        for hedge in [Position::new(1, 0), Position::new(1, 1)] {
            let board = Board::new(5, 5, [hedge]);
            assert!(!has_line_of_sight(&board, a, b), "blocked A→B by {hedge:?}");
            assert!(!has_line_of_sight(&board, b, a), "blocked B→A by {hedge:?}");
        }
    }

    #[test]
    fn perfect_diagonal_does_not_clip_the_other_corner() {
        // (0,0) → (2,2) goes through the vertex at (1,1). The hedge at
        // (1,0) shares that vertex but is not on the center-to-center line.
        let board = Board::new(5, 5, [Position::new(1, 0)]);
        assert!(has_line_of_sight(
            &board,
            Position::new(0, 0),
            Position::new(2, 2)
        ));
        // A hedge sitting ON the diagonal does block.
        let board = Board::new(5, 5, [Position::new(1, 1)]);
        assert!(!has_line_of_sight(
            &board,
            Position::new(0, 0),
            Position::new(2, 2)
        ));
    }

    #[test]
    fn tiles_on_a_shallow_line_include_both_step_axes() {
        assert_eq!(
            tiles_on_line(Position::new(0, 0), Position::new(2, 1)),
            vec![
                Position::new(0, 0),
                Position::new(1, 0),
                Position::new(1, 1),
                Position::new(2, 1),
            ]
        );
    }

    #[test]
    fn cover_can_reduce_damage_to_zero() {
        let board = Board::new(3, 3, [Position::new(1, 1)]);
        let rules = Rules {
            attack_damage: 2,
            cover_damage_reduction: 5,
            ..Rules::standard()
        };
        assert_eq!(damage_after_cover(&rules, &board, Position::new(1, 1)), 0);
    }
}
