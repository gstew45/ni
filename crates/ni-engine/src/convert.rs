//! Explicit adapters between authoritative game types and wire types.

use ni_game::{Chapter as GameChapter, MatchState, Order as GameOrder, Position as GamePosition};

use ni_proto::ni::v1::{
    BattlefieldView, BoardLayout, Chapter as ProtoChapter, Knight as ProtoKnight,
    KnightOrder as ProtoOrder, NewMatchRequest, Position as ProtoPosition, Rules as ProtoRules,
};

use crate::policy::TimeControl;

pub fn chapter_to_proto(chapter: GameChapter) -> i32 {
    match chapter {
        GameChapter::A => ProtoChapter::A as i32,
        GameChapter::B => ProtoChapter::B as i32,
    }
}

fn position_to_proto(position: GamePosition) -> ProtoPosition {
    ProtoPosition {
        x: position.x,
        y: position.y,
    }
}

pub fn board_layout(state: &MatchState) -> BoardLayout {
    let mut shrubbery: Vec<_> = state.board.shrubbery.iter().copied().collect();
    shrubbery.sort();

    BoardLayout {
        width: state.board.width,
        height: state.board.height,
        shrubbery: shrubbery.into_iter().map(position_to_proto).collect(),
    }
}

pub fn rules_to_proto(state: &MatchState, time: TimeControl) -> ProtoRules {
    ProtoRules {
        knight_hp: state.rules.knight_hp,
        move_range: state.rules.move_range,
        attack_range: state.rules.attack_range,
        attack_damage: state.rules.attack_damage,
        cover_damage_reduction: state.rules.cover_damage_reduction,
        turn_cap: state.rules.turn_cap,
        turn_deadline_ms: time.turn_deadline_ms(),
        timeout_strike_limit: time.strike_limit,
    }
}

pub fn new_match_request(
    state: &MatchState,
    match_id: &str,
    chapter: GameChapter,
    time: TimeControl,
) -> NewMatchRequest {
    NewMatchRequest {
        match_id: match_id.to_string(),
        chapter: chapter_to_proto(chapter),
        board: Some(board_layout(state)),
        rules: Some(rules_to_proto(state, time)),
        turn: 0,
    }
}

pub fn resume_match_request(
    state: &MatchState,
    match_id: &str,
    chapter: GameChapter,
    time: TimeControl,
) -> NewMatchRequest {
    NewMatchRequest {
        turn: state.turn,
        ..new_match_request(state, match_id, chapter, time)
    }
}

pub fn battlefield_view(state: &MatchState) -> BattlefieldView {
    let board = board_layout(state);

    BattlefieldView {
        width: board.width,
        height: board.height,
        shrubbery: board.shrubbery,
        knights: state
            .knights
            .iter()
            .map(|knight| ProtoKnight {
                unit_id: knight.id.clone(),
                chapter: chapter_to_proto(knight.chapter),
                position: Some(position_to_proto(knight.pos)),
                hp: knight.hp,
                move_range: knight.move_range,
                attack_range: knight.attack_range,
                attack_damage: knight.damage,
            })
            .collect(),
        to_act: chapter_to_proto(state.to_act),
        turn: state.turn,
        time_remaining_ms: 0,
    }
}

pub fn order_from_proto(order: ProtoOrder) -> GameOrder {
    GameOrder {
        unit_id: order.unit_id,
        move_to: order.move_to.map(|position| GamePosition {
            x: position.x,
            y: position.y,
        }),
        attack_target: order.attack_target,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ni_game::{apply_orders, IllegalReason, OrderResult, Rules};

    #[test]
    fn view_contains_sorted_shrubbery_and_all_knights() {
        let mut state = ni_game::standard_match(Rules::standard());
        state.knights[0].hp = 0;

        let view = battlefield_view(&state);

        assert_eq!(view.knights.len(), state.knights.len());
        assert_eq!(view.knights[0].unit_id, "A1");
        assert_eq!(view.knights[0].hp, 0);
        assert_eq!(view.knights[0].position, Some(ProtoPosition { x: 1, y: 1 }));
        assert!(view
            .shrubbery
            .windows(2)
            .all(|pair| { (pair[0].x, pair[0].y) <= (pair[1].x, pair[1].y) }));
    }

    #[test]
    fn both_chapters_map_to_the_wire_enum() {
        assert_eq!(chapter_to_proto(GameChapter::A), ProtoChapter::A as i32);
        assert_eq!(chapter_to_proto(GameChapter::B), ProtoChapter::B as i32);
    }

    #[test]
    fn order_option_fields_survive_conversion() {
        let order = order_from_proto(ProtoOrder {
            unit_id: "A1".to_string(),
            move_to: Some(ProtoPosition { x: 2, y: 1 }),
            attack_target: Some("B1".to_string()),
        });

        assert_eq!(order.unit_id, "A1");
        assert_eq!(order.move_to, Some(GamePosition { x: 2, y: 1 }));
        assert_eq!(order.attack_target.as_deref(), Some("B1"));
    }

    #[test]
    fn adapter_preserves_illegal_positions_for_game_validation() {
        let state = ni_game::standard_match(Rules::standard());
        let order = order_from_proto(ProtoOrder {
            unit_id: "A1".to_string(),
            move_to: Some(ProtoPosition { x: 999, y: 999 }),
            attack_target: None,
        });

        let (_, outcomes) = apply_orders(state, GameChapter::A, &[order]);

        assert_eq!(
            outcomes[0].result,
            OrderResult::Illegal {
                reason: IllegalReason::DestinationOob,
            }
        );
    }

    #[test]
    fn a_fresh_match_starts_at_turn_zero_and_a_resume_does_not() {
        let mut state = ni_game::standard_match(Rules::standard());
        state.turn = 7;
        let time = TimeControl {
            turn_deadline: std::time::Duration::from_millis(250),
            strike_limit: 3,
        };

        let fresh = new_match_request(&state, "m3-demo", GameChapter::A, time);
        let resumed = resume_match_request(&state, "m3-demo", GameChapter::A, time);

        assert_eq!(fresh.turn, 0);
        assert_eq!(resumed.turn, 7);
        assert_eq!(resumed.board, fresh.board);
        assert_eq!(resumed.rules.unwrap().turn_deadline_ms, 250);
    }
}
