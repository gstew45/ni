//! Order validation and resolution — the heart of M1.
//!
//! The engine is the sole authority: bots propose, this module disposes.
//! Nothing in an [`Order`] is trusted. Illegal input is a defined outcome,
//! never a panic.

use std::collections::HashSet;

use crate::los::{damage_after_cover, has_line_of_sight};
use crate::types::{Chapter, IllegalReason, MatchState, Order, OrderOutcome, OrderResult};

/// Apply `orders` for `chapter` against `state`, stopping at the first
/// illegal order. Returns the mutated state and one outcome per input order.
///
/// Partial application is the design choice (implementation-plan Q1):
/// orders 1..k-1 stick, order k is [`OrderResult::Illegal`], k+1..n are
/// [`OrderResult::NotReached`]. That property is what makes a retried
/// `GetOrders` in M3 an interesting failure, not a crash.
///
/// This function does **not** increment `turn` or flip `to_act` — call
/// [`end_turn`] after you've logged the outcomes. That split keeps a
/// failed-illegal turn and a successful turn on the same code path.
pub fn apply_orders(
    mut state: MatchState,
    chapter: Chapter,
    orders: &[Order],
) -> (MatchState, Vec<OrderOutcome>) {
    debug_assert_eq!(chapter, state.to_act, "engine must pass the acting chapter");

    let mut acted = HashSet::new();
    let mut halted = false;
    let mut outcomes = Vec::with_capacity(orders.len());

    for order in orders {
        if halted {
            outcomes.push(OrderOutcome {
                order: order.clone(),
                result: OrderResult::NotReached,
            });
            continue;
        }
        match apply_one(&mut state, chapter, order, &mut acted) {
            Ok(damage_dealt) => outcomes.push(OrderOutcome {
                order: order.clone(),
                result: OrderResult::Applied { damage_dealt },
            }),
            Err(reason) => {
                outcomes.push(OrderOutcome {
                    order: order.clone(),
                    result: OrderResult::Illegal { reason },
                });
                halted = true;
            }
        }
    }

    (state, outcomes)
}

/// Validate and apply a single order against the state-as-mutated-so-far.
///
/// Copy values out of `state.knights[i]` *before* any mutation so we never
/// hold two borrows of the same `Vec`. Indexing `knights[i]` then
/// `knights[t]` in separate statements is allowed — each mutable borrow
/// ends at the end of the statement.
fn apply_one(
    state: &mut MatchState,
    chapter: Chapter,
    order: &Order,
    acted: &mut HashSet<String>,
) -> Result<u32, IllegalReason> {
    let i = state
        .knights
        .iter()
        .position(|k| k.id == order.unit_id)
        .ok_or(IllegalReason::UnknownUnit)?;

    if state.knights[i].chapter != chapter {
        return Err(IllegalReason::NotYourKnight);
    }
    if !state.knights[i].is_alive() {
        return Err(IllegalReason::Dead);
    }
    if acted.contains(&state.knights[i].id) {
        return Err(IllegalReason::AlreadyActed);
    }

    let from = state.knights[i].pos;
    let dest = order.move_to.unwrap_or(from);
    if dest != from {
        if !state.board.in_bounds(dest) {
            return Err(IllegalReason::DestinationOob);
        }
        if from.manhattan(dest) > state.knights[i].move_range {
            return Err(IllegalReason::MoveOutOfRange);
        }
        // `order.unit_id` is not borrowed from `state`, so this check and
        // the assignment below don't fight the borrow checker.
        if state
            .living_at(dest)
            .is_some_and(|occupant| occupant.id != order.unit_id)
        {
            return Err(IllegalReason::DestinationOccupied);
        }
        state.knights[i].pos = dest;
    }

    let mut damage_dealt = 0;
    if let Some(target_id) = &order.attack_target {
        let t = state
            .knights
            .iter()
            .position(|k| k.id == *target_id)
            .ok_or(IllegalReason::TargetUnknown)?;

        if !state.knights[t].is_alive() {
            return Err(IllegalReason::TargetDead);
        }
        if t == i || state.knights[t].chapter == chapter {
            return Err(IllegalReason::TargetNotEnemy);
        }

        let origin = state.knights[i].pos;
        let target_pos = state.knights[t].pos;
        if origin.manhattan(target_pos) > state.knights[i].attack_range {
            return Err(IllegalReason::TargetOutOfRange);
        }
        if !has_line_of_sight(&state.board, origin, target_pos) {
            return Err(IllegalReason::NoLineOfSight);
        }

        damage_dealt = damage_after_cover(&state.rules, &state.board, target_pos);
        state.knights[t].hp = state.knights[t].hp.saturating_sub(damage_dealt);
    }

    acted.insert(order.unit_id.clone());
    Ok(damage_dealt)
}

/// Flip `to_act` and increment `turn`. Call after [`apply_orders`], even
/// when the turn halted on an illegal order — the chapter still spent its
/// action.
///
/// `turn` is the chapter-action about to happen, starting at 1.
/// [`match_status`] reports [`EndReason::TurnCap`] when `turn > rules.turn_cap`
/// — i.e. after `turn_cap` calls to this function. Check status after
/// [`apply_orders`] (to catch elimination) *and* after this (to catch the cap).
pub fn end_turn(state: &mut MatchState) {
    state.to_act = state.to_act.opponent();
    state.turn += 1;
}

/// Whether the match is over. Forfeits (timeout, crash, protocol) are
/// engine decisions and do not live here.
///
/// Elimination is checked *before* the turn cap so a killing blow on the
/// last allowed turn still counts as a win.
pub fn match_status(state: &MatchState) -> MatchStatus {
    let a_alive = state.living(Chapter::A).count();
    let b_alive = state.living(Chapter::B).count();

    match (a_alive, b_alive) {
        (0, 0) => MatchStatus::Draw {
            reason: EndReason::Elimination,
        },
        (0, _) => MatchStatus::Winner {
            chapter: Chapter::B,
            reason: EndReason::Elimination,
        },
        (_, 0) => MatchStatus::Winner {
            chapter: Chapter::A,
            reason: EndReason::Elimination,
        },
        _ if state.turn > state.rules.turn_cap => {
            match state.total_hp(Chapter::A).cmp(&state.total_hp(Chapter::B)) {
                std::cmp::Ordering::Greater => MatchStatus::Winner {
                    chapter: Chapter::A,
                    reason: EndReason::TurnCap,
                },
                std::cmp::Ordering::Less => MatchStatus::Winner {
                    chapter: Chapter::B,
                    reason: EndReason::TurnCap,
                },
                std::cmp::Ordering::Equal => MatchStatus::Draw {
                    reason: EndReason::TurnCap,
                },
            }
        }
        _ => MatchStatus::InProgress,
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MatchStatus {
    InProgress,
    Winner { chapter: Chapter, reason: EndReason },
    Draw { reason: EndReason },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EndReason {
    Elimination,
    TurnCap,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::standard_match;
    use crate::types::{Board, Knight, Position, Rules};

    fn order(unit: &str, dest: Option<(u32, u32)>, target: Option<&str>) -> Order {
        Order {
            unit_id: unit.to_string(),
            move_to: dest.map(|(x, y)| Position::new(x, y)),
            attack_target: target.map(str::to_string),
        }
    }

    fn applied(damage_dealt: u32) -> OrderResult {
        OrderResult::Applied { damage_dealt }
    }

    fn illegal(reason: IllegalReason) -> OrderResult {
        OrderResult::Illegal { reason }
    }

    fn spawn(id: &str, chapter: Chapter, pos: (u32, u32), rules: &Rules) -> Knight {
        Knight {
            id: id.to_string(),
            chapter,
            pos: Position::new(pos.0, pos.1),
            hp: rules.knight_hp,
            move_range: rules.move_range,
            attack_range: rules.attack_range,
            damage: rules.attack_damage,
        }
    }

    /// Two knights on an empty 5×5, A to act. Optional shrubbery.
    fn duel(
        a: (u32, u32),
        b: (u32, u32),
        shrubbery: impl IntoIterator<Item = Position>,
    ) -> MatchState {
        let rules = Rules::standard();
        MatchState {
            board: Board::new(5, 5, shrubbery),
            knights: vec![
                spawn("A1", Chapter::A, a, &rules),
                spawn("B1", Chapter::B, b, &rules),
            ],
            turn: 1,
            to_act: Chapter::A,
            rules,
        }
    }

    #[test]
    fn end_turn_flips_chapter_and_increments() {
        let mut state = standard_match(Rules::standard());
        assert_eq!(state.to_act, Chapter::A);
        assert_eq!(state.turn, 1);
        end_turn(&mut state);
        assert_eq!(state.to_act, Chapter::B);
        assert_eq!(state.turn, 2);
        end_turn(&mut state);
        assert_eq!(state.to_act, Chapter::A);
        assert_eq!(state.turn, 3);
    }

    #[test]
    fn empty_order_list_is_a_legal_pass() {
        let state = standard_match(Rules::standard());
        let before = state.clone();
        let (after, outcomes) = apply_orders(state, Chapter::A, &[]);
        assert!(outcomes.is_empty());
        assert_eq!(after.knights, before.knights);
    }

    #[test]
    fn legal_step_moves_the_knight() {
        let state = standard_match(Rules::standard());
        let from = state.knight("A1").unwrap().pos;
        let (state, outcomes) = apply_orders(state, Chapter::A, &[order("A1", Some((1, 2)), None)]);
        assert_eq!(outcomes[0].result, applied(0));
        assert_eq!(state.knight("A1").unwrap().pos, Position::new(1, 2));
        assert_ne!(state.knight("A1").unwrap().pos, from);
    }

    #[test]
    fn movement_is_not_pathfinding() {
        // Hedge between A and dest; jumping it is legal because movement
        // only checks destination, not the path.
        let state = duel((0, 0), (0, 4), [Position::new(1, 0)]);
        let (state, outcomes) = apply_orders(state, Chapter::A, &[order("A1", Some((2, 0)), None)]);
        assert_eq!(outcomes[0].result, applied(0));
        assert_eq!(state.knight("A1").unwrap().pos, Position::new(2, 0));
    }

    #[test]
    fn first_illegal_stops_the_turn() {
        let state = standard_match(Rules::standard());
        let a2_from = state.knight("A2").unwrap().pos;
        let (state, outcomes) = apply_orders(
            state,
            Chapter::A,
            &[
                order("A1", Some((99, 99)), None),
                order("A2", Some((2, 3)), None),
            ],
        );
        assert_eq!(outcomes[0].result, illegal(IllegalReason::DestinationOob));
        assert_eq!(outcomes[1].result, OrderResult::NotReached);
        assert_eq!(state.knight("A2").unwrap().pos, a2_from);
    }

    #[test]
    fn cannot_order_the_other_chapter() {
        let state = standard_match(Rules::standard());
        let (_, outcomes) = apply_orders(state, Chapter::A, &[order("B1", Some((7, 1)), None)]);
        assert_eq!(outcomes[0].result, illegal(IllegalReason::NotYourKnight));
    }

    #[test]
    fn second_order_for_same_knight_is_already_acted() {
        let state = standard_match(Rules::standard());
        let (state, outcomes) = apply_orders(
            state,
            Chapter::A,
            &[
                order("A1", Some((1, 2)), None),
                order("A1", Some((1, 3)), None),
            ],
        );
        assert_eq!(outcomes[0].result, applied(0));
        assert_eq!(outcomes[1].result, illegal(IllegalReason::AlreadyActed));
        assert_eq!(state.knight("A1").unwrap().pos, Position::new(1, 2));
    }

    #[test]
    fn attack_in_range_reduces_hp() {
        let state = duel((0, 0), (1, 0), []);
        let (state, outcomes) = apply_orders(state, Chapter::A, &[order("A1", None, Some("B1"))]);
        assert_eq!(outcomes[0].result, applied(Rules::standard().attack_damage));
        assert_eq!(
            state.knight("B1").unwrap().hp,
            Rules::standard().knight_hp - Rules::standard().attack_damage
        );
        assert_eq!(state.knight("A1").unwrap().hp, Rules::standard().knight_hp);
    }

    #[test]
    fn attack_after_move_uses_new_position() {
        // B is 3 east — out of attack_range 2 from (0,0), in range after a step.
        let state = duel((0, 0), (3, 0), []);
        let (state, outcomes) =
            apply_orders(state, Chapter::A, &[order("A1", Some((1, 0)), Some("B1"))]);
        assert_eq!(outcomes[0].result, applied(Rules::standard().attack_damage));
        assert_eq!(state.knight("A1").unwrap().pos, Position::new(1, 0));
        assert_eq!(
            state.knight("B1").unwrap().hp,
            Rules::standard().knight_hp - Rules::standard().attack_damage
        );
    }

    #[test]
    fn cover_reduces_attack_damage() {
        let hedge = Position::new(1, 0);
        let state = duel((0, 0), (1, 0), [hedge]);
        let rules = Rules::standard();
        let (state, outcomes) = apply_orders(state, Chapter::A, &[order("A1", None, Some("B1"))]);
        let want = rules.attack_damage - rules.cover_damage_reduction;
        assert_eq!(outcomes[0].result, applied(want));
        assert_eq!(state.knight("B1").unwrap().hp, rules.knight_hp - want);
    }

    #[test]
    fn corpses_do_not_block_movement() {
        let mut state = duel((0, 0), (1, 0), []);
        state.knights.iter_mut().find(|k| k.id == "B1").unwrap().hp = 0;
        let (state, outcomes) = apply_orders(state, Chapter::A, &[order("A1", Some((1, 0)), None)]);
        assert_eq!(outcomes[0].result, applied(0));
        assert_eq!(state.knight("A1").unwrap().pos, Position::new(1, 0));
    }

    #[test]
    fn movement_and_attack_illegal_reasons() {
        let cases = [
            (
                "unknown unit",
                duel((0, 0), (1, 0), []),
                order("A9", Some((0, 1)), None),
                IllegalReason::UnknownUnit,
            ),
            (
                "dead knight",
                {
                    let mut s = duel((0, 0), (1, 0), []);
                    s.knights[0].hp = 0;
                    s
                },
                order("A1", Some((0, 1)), None),
                IllegalReason::Dead,
            ),
            (
                "move out of range",
                duel((0, 0), (4, 4), []),
                order("A1", Some((4, 0)), None), // manhattan 4 > 3
                IllegalReason::MoveOutOfRange,
            ),
            (
                "destination occupied",
                duel((0, 0), (1, 0), []),
                order("A1", Some((1, 0)), None),
                IllegalReason::DestinationOccupied,
            ),
            (
                "target unknown",
                duel((0, 0), (1, 0), []),
                order("A1", None, Some("B9")),
                IllegalReason::TargetUnknown,
            ),
            (
                "target dead",
                {
                    let mut s = duel((0, 0), (1, 0), []);
                    s.knights[1].hp = 0;
                    s
                },
                order("A1", None, Some("B1")),
                IllegalReason::TargetDead,
            ),
            (
                "friendly fire",
                {
                    let rules = Rules::standard();
                    MatchState {
                        board: Board::new(5, 5, []),
                        knights: vec![
                            spawn("A1", Chapter::A, (0, 0), &rules),
                            spawn("A2", Chapter::A, (1, 0), &rules),
                        ],
                        turn: 1,
                        to_act: Chapter::A,
                        rules,
                    }
                },
                order("A1", None, Some("A2")),
                IllegalReason::TargetNotEnemy,
            ),
            (
                "target out of range",
                duel((0, 0), (3, 0), []),
                order("A1", None, Some("B1")), // manhattan 3 > attack_range 2
                IllegalReason::TargetOutOfRange,
            ),
            (
                "no line of sight",
                duel((0, 0), (2, 0), [Position::new(1, 0)]),
                order("A1", None, Some("B1")),
                IllegalReason::NoLineOfSight,
            ),
        ];

        for (name, state, ord, want) in cases {
            let (_, outcomes) = apply_orders(state, Chapter::A, std::slice::from_ref(&ord));
            assert_eq!(outcomes[0].result, illegal(want), "{name}");
        }
    }

    #[test]
    fn match_status_elimination() {
        let mut state = standard_match(Rules::standard());
        for k in &mut state.knights {
            if k.chapter == Chapter::B {
                k.hp = 0;
            }
        }
        assert_eq!(
            match_status(&state),
            MatchStatus::Winner {
                chapter: Chapter::A,
                reason: EndReason::Elimination,
            }
        );
    }

    #[test]
    fn killing_blow_on_last_turn_is_elimination_not_turn_cap() {
        let mut state = duel((0, 0), (1, 0), []);
        state.rules.turn_cap = 1;
        state.knights[1].hp = state.rules.attack_damage;
        let (state, _) = apply_orders(state, Chapter::A, &[order("A1", None, Some("B1"))]);
        assert_eq!(state.turn, 1);
        assert_eq!(
            match_status(&state),
            MatchStatus::Winner {
                chapter: Chapter::A,
                reason: EndReason::Elimination,
            }
        );
    }

    #[test]
    fn turn_cap_decides_on_surviving_hp() {
        let mut state = standard_match(Rules {
            turn_cap: 2,
            ..Rules::standard()
        });
        assert_eq!(match_status(&state), MatchStatus::InProgress);
        end_turn(&mut state); // turn 2 == cap, still in progress
        assert_eq!(match_status(&state), MatchStatus::InProgress);
        end_turn(&mut state); // turn 3 > 2
        assert_eq!(
            match_status(&state),
            MatchStatus::Draw {
                reason: EndReason::TurnCap,
            }
        );

        state.knights.iter_mut().find(|k| k.id == "A1").unwrap().hp = 9;
        assert_eq!(
            match_status(&state),
            MatchStatus::Winner {
                chapter: Chapter::B,
                reason: EndReason::TurnCap,
            }
        );
    }

    #[test]
    fn golden_short_scripted_match() {
        let rules = Rules::standard();
        let spawn_state = standard_match(rules);
        let mut state = spawn_state.clone();

        let (next, outcomes) = apply_orders(state, Chapter::A, &[order("A2", Some((4, 3)), None)]);
        assert_eq!(outcomes[0].result, applied(0));
        state = next;
        end_turn(&mut state);

        let (next, outcomes) = apply_orders(state, Chapter::B, &[order("B2", Some((5, 3)), None)]);
        assert_eq!(outcomes[0].result, applied(0));
        state = next;
        end_turn(&mut state);

        let (next, outcomes) = apply_orders(state, Chapter::A, &[order("A2", None, Some("B2"))]);
        assert_eq!(outcomes[0].result, applied(rules.attack_damage));
        state = next;
        end_turn(&mut state);

        assert_eq!(state.knight("A2").unwrap().pos, Position::new(4, 3));
        assert_eq!(state.knight("B2").unwrap().pos, Position::new(5, 3));
        assert_eq!(
            state.knight("B2").unwrap().hp,
            rules.knight_hp - rules.attack_damage
        );
        for id in ["A1", "A3", "A4", "B1", "B3", "B4"] {
            let now = state.knight(id).unwrap();
            let then = spawn_state.knight(id).unwrap();
            assert_eq!(now.pos, then.pos, "{id} moved");
            assert_eq!(now.hp, then.hp, "{id} took damage");
        }
        assert_eq!(match_status(&state), MatchStatus::InProgress);
        assert_eq!(state.turn, 4);
        assert_eq!(state.to_act, Chapter::B);
    }

    #[test]
    fn golden_duel_to_elimination() {
        let rules = Rules {
            knight_hp: 8,
            attack_damage: 4,
            attack_range: 1,
            move_range: 1,
            cover_damage_reduction: 0,
            turn_cap: 100,
        };
        let mut state = MatchState {
            board: Board::new(3, 1, []),
            knights: vec![
                spawn("A1", Chapter::A, (0, 0), &rules),
                spawn("B1", Chapter::B, (1, 0), &rules),
            ],
            turn: 1,
            to_act: Chapter::A,
            rules,
        };

        // 8 hp, 4 dmg: A hits, B hits, A hits → B dies.
        let script = [
            (Chapter::A, order("A1", None, Some("B1")), 4, 8, 4),
            (Chapter::B, order("B1", None, Some("A1")), 4, 4, 4),
            (Chapter::A, order("A1", None, Some("B1")), 4, 4, 0),
        ];
        for (chapter, ord, dmg, a_hp, b_hp) in script {
            let (next, outcomes) = apply_orders(state, chapter, &[ord]);
            assert_eq!(outcomes[0].result, applied(dmg));
            state = next;
            assert_eq!(state.knight("A1").unwrap().hp, a_hp);
            assert_eq!(state.knight("B1").unwrap().hp, b_hp);
            if match_status(&state) == MatchStatus::InProgress {
                end_turn(&mut state);
            }
        }

        assert_eq!(
            match_status(&state),
            MatchStatus::Winner {
                chapter: Chapter::A,
                reason: EndReason::Elimination,
            }
        );
        assert!(!state.knight("B1").unwrap().is_alive());
        assert_eq!(state.knight("A1").unwrap().pos, Position::new(0, 0));
        assert_eq!(state.knight("B1").unwrap().pos, Position::new(1, 0));
    }
}
