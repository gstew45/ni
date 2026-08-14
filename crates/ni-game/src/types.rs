//! Core types for the rules engine.
//!
//! These are *game* types, not proto types. Field names line up with
//! `proto/ni/v1/ni.proto` so M2's conversion is boring (the goal), but this
//! crate does not depend on `ni-proto`.
//!
//! # Rust notes (read these)
//!
//! - [`Position`] is `Copy`. Passing it around is like passing two `u32`s;
//!   you never need `.clone()`.
//! - [`Knight`] is `Clone` but not `Copy` because `id: String` owns a heap
//!   buffer. Clone it only when you must — prefer `&Knight`.
//! - `#[derive(Hash, Eq, PartialEq)]` on [`Position`] lets it live in a
//!   `HashSet`, which makes "is this tile shrubbery?" O(1).
//! - Proto uses `uint32`; we keep `u32` here so the M2 mapping is 1:1.
//!   Convert to `usize` only if you index a `Vec`.

use std::collections::HashSet;

/// Grid coordinate. Origin is top-left, x grows right, y grows down —
/// the same convention the text renderer in M2 will use.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct Position {
    pub x: u32,
    pub y: u32,
}

impl Position {
    pub fn new(x: u32, y: u32) -> Self {
        Self { x, y }
    }

    /// Manhattan distance: `|dx| + |dy|`. This is the metric for both
    /// movement range and attack range.
    ///
    /// Hint: `u32::abs_diff` avoids any signed-cast dance.
    pub fn manhattan(self, other: Self) -> u32 {
        self.x.abs_diff(other.x) + self.y.abs_diff(other.y)
    }
}

/// A seat in the match. Armies are mirrored; any bot can play either side.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum Chapter {
    A,
    B,
}

impl Chapter {
    pub fn opponent(self) -> Self {
        match self {
            Chapter::A => Chapter::B,
            Chapter::B => Chapter::A,
        }
    }
}

/// Numeric knobs for a match. The engine is the single source of these;
/// bots receive them in `NewMatch` and must not assume defaults.
///
/// `turn_deadline_ms` and `timeout_strike_limit` live on the proto `Rules`
/// message too, but they are *engine* concerns (gRPC deadlines, strike
/// counters). They don't belong in the pure rules crate.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Rules {
    pub knight_hp: u32,
    pub move_range: u32,
    pub attack_range: u32,
    pub attack_damage: u32,
    /// Subtracted from [`Self::attack_damage`] when the *target* stands in
    /// shrubbery. Attacker-in-shrubbery does not get a bonus.
    pub cover_damage_reduction: u32,
    /// After this many chapter-actions the match is a draw, decided on
    /// surviving total HP. See [`crate::end_turn`] for how `turn` counts.
    pub turn_cap: u32,
}

impl Rules {
    /// Sensible v1 defaults. Change these only via `NewMatch` config later;
    /// tests that care about numbers should construct [`Rules`] explicitly.
    pub fn standard() -> Self {
        Self {
            knight_hp: 10,
            move_range: 3,
            attack_range: 2,
            attack_damage: 4,
            cover_damage_reduction: 2,
            turn_cap: 100,
        }
    }
}

/// The grid plus the shrubbery set.
///
/// Movement does **not** pathfind: a knight may land on any in-bounds,
/// unoccupied tile within Manhattan range, jumping shrubbery and units.
/// Shrubbery only matters for line of sight and cover.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Board {
    pub width: u32,
    pub height: u32,
    pub shrubbery: HashSet<Position>,
}

impl Board {
    pub fn new(width: u32, height: u32, shrubbery: impl IntoIterator<Item = Position>) -> Self {
        Self {
            width,
            height,
            shrubbery: shrubbery.into_iter().collect(),
        }
    }

    pub fn in_bounds(&self, pos: Position) -> bool {
        pos.x < self.width && pos.y < self.height
    }

    pub fn has_shrubbery(&self, pos: Position) -> bool {
        self.shrubbery.contains(&pos)
    }
}

/// One unit. Dead knights stay in [`MatchState::knights`] with `hp == 0`
/// so ids remain stable for the view; they do not occupy a tile and cannot
/// act.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Knight {
    /// Stable id, e.g. `"A1"`..`"A4"`, `"B1"`..`"B4"`.
    pub id: String,
    pub chapter: Chapter,
    pub pos: Position,
    pub hp: u32,
    pub move_range: u32,
    pub attack_range: u32,
    pub damage: u32,
}

impl Knight {
    pub fn is_alive(&self) -> bool {
        self.hp > 0
    }
}

/// Authoritative match state. The engine holds one of these; bots never
/// see it — they see a `BattlefieldView` projection built in M2.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MatchState {
    pub board: Board,
    pub knights: Vec<Knight>,
    /// Increments once per chapter-action (once per `GetOrders` in M2).
    pub turn: u32,
    pub to_act: Chapter,
    pub rules: Rules,
}

impl MatchState {
    pub fn knight(&self, id: &str) -> Option<&Knight> {
        self.knights.iter().find(|k| k.id == id)
    }

    /// Living knight occupying `pos`, if any. Corpses don't block tiles.
    pub fn living_at(&self, pos: Position) -> Option<&Knight> {
        self.knights.iter().find(|k| k.is_alive() && k.pos == pos)
    }

    pub fn living(&self, chapter: Chapter) -> impl Iterator<Item = &Knight> {
        self.knights
            .iter()
            .filter(move |k| k.chapter == chapter && k.is_alive())
    }

    /// Sum of HP for a chapter, including corpses (which contribute 0).
    pub fn total_hp(&self, chapter: Chapter) -> u32 {
        self.knights
            .iter()
            .filter(|k| k.chapter == chapter)
            .map(|k| k.hp)
            .sum()
    }
}

/// One knight's action for the turn: optionally move, then optionally
/// attack. Both fields missing is a no-op (legal, wastes the knight's act).
///
/// The list order *is* the resolution order — that's what makes partial
/// application interesting for the gRPC retry post later.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Order {
    pub unit_id: String,
    pub move_to: Option<Position>,
    pub attack_target: Option<String>,
}

/// How a single order fared. Mirrors `ni.v1.OrderOutcome` / `OrderResult`,
/// but `reason` is a real enum rather than a string — stringify at the
/// proto boundary in M2.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OrderOutcome {
    pub order: Order,
    pub result: OrderResult,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum OrderResult {
    Applied {
        /// 0 when the order was move-only, or an attack that cover fully
        /// absorbed.
        damage_dealt: u32,
    },
    /// This order was illegal and **stopped the turn**. Remaining orders
    /// in the list become [`OrderResult::NotReached`].
    Illegal {
        reason: IllegalReason,
    },
    NotReached,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IllegalReason {
    /// No knight with that `unit_id`.
    UnknownUnit,
    /// Knight belongs to the other chapter (the bot tried to steer an enemy).
    NotYourKnight,
    /// `hp == 0`.
    Dead,
    /// Same knight appeared twice in this turn's list.
    AlreadyActed,
    /// `move_to` is off the board.
    DestinationOob,
    /// Manhattan(current, dest) > move_range.
    MoveOutOfRange,
    /// A living knight already stands on `move_to` (and it isn't the mover
    /// staying put).
    DestinationOccupied,
    /// `attack_target` id doesn't exist.
    TargetUnknown,
    /// Target has `hp == 0`.
    TargetDead,
    /// Target is the attacker, or a teammate. No friendly fire in v1.
    TargetNotEnemy,
    /// After the move, Manhattan > attack_range.
    TargetOutOfRange,
    /// A shrubbery tile sits on the line between attacker and target.
    NoLineOfSight,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manhattan_is_abs_dx_plus_abs_dy() {
        let cases = [
            (Position::new(0, 0), Position::new(0, 0), 0),
            (Position::new(0, 0), Position::new(3, 4), 7),
            (Position::new(5, 5), Position::new(4, 6), 2),
            (Position::new(2, 1), Position::new(0, 0), 3),
        ];
        for (a, b, want) in cases {
            assert_eq!(a.manhattan(b), want);
            assert_eq!(b.manhattan(a), want, "manhattan should be symmetric");
        }
    }

    #[test]
    fn opponent_swaps_chapters() {
        assert_eq!(Chapter::A.opponent(), Chapter::B);
        assert_eq!(Chapter::B.opponent(), Chapter::A);
    }

    #[test]
    fn in_bounds_is_half_open() {
        let board = Board::new(10, 10, []);
        assert!(board.in_bounds(Position::new(0, 0)));
        assert!(board.in_bounds(Position::new(9, 9)));
        assert!(!board.in_bounds(Position::new(10, 0)));
        assert!(!board.in_bounds(Position::new(0, 10)));
    }

    #[test]
    fn corpses_do_not_occupy_tiles() {
        let mut state = crate::standard_match(Rules::standard());
        let pos = state.knight("A1").unwrap().pos;
        assert!(state.living_at(pos).is_some());
        state.knights.iter_mut().find(|k| k.id == "A1").unwrap().hp = 0;
        assert!(state.living_at(pos).is_none());
    }
}
