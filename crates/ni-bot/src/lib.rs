pub mod server;

pub use server::serve;

use std::collections::{HashMap, HashSet};

use ni_proto::{
    ni::v1::{
        bot_service_server::BotService, BattlefieldView, Chapter, GetOrdersRequest,
        GetOrdersResponse, IdentifyRequest, IdentifyResponse, Knight, KnightOrder,
        MatchEndedRequest, MatchEndedResponse, NewMatchRequest, NewMatchResponse, Position,
        SubmitReplayRequest, SubmitReplayResponse,
    },
    PROTOCOL_VERSION, SHRUBBERY_REQUIRED,
};
use tokio::sync::RwLock;
use tonic::{Request, Response, Status};

#[derive(Clone, Copy)]
struct MatchInfo {
    chapter: Chapter,
}

#[derive(Default)]
pub struct ReferenceBot {
    matches: RwLock<HashMap<String, MatchInfo>>,
}

#[tonic::async_trait]
impl BotService for ReferenceBot {
    async fn identify(
        &self,
        _request: Request<IdentifyRequest>,
    ) -> Result<Response<IdentifyResponse>, Status> {
        Ok(Response::new(IdentifyResponse {
            name: "reference-bot".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: PROTOCOL_VERSION,
        }))
    }

    async fn new_match(
        &self,
        request: Request<NewMatchRequest>,
    ) -> Result<Response<NewMatchResponse>, Status> {
        let NewMatchRequest {
            match_id,
            chapter,
            board,
            rules,
            ..
        } = request.into_inner();

        if match_id.is_empty() {
            return Err(Status::invalid_argument("match_id is required"));
        }

        let chapter =
            Chapter::try_from(chapter).map_err(|_| Status::invalid_argument("unknown chapter"))?;

        if chapter == Chapter::Unspecified {
            return Err(Status::invalid_argument("chapter is required"));
        }

        let _board = board.ok_or_else(|| Status::invalid_argument("board is required"))?;
        let _rules = rules.ok_or_else(|| Status::invalid_argument("rules are required"))?;

        self.matches
            .write()
            .await
            .insert(match_id, MatchInfo { chapter });

        Ok(Response::new(NewMatchResponse {}))
    }

    async fn get_orders(
        &self,
        request: Request<GetOrdersRequest>,
    ) -> Result<Response<GetOrdersResponse>, Status> {
        let request = request.into_inner();

        let info = self
            .matches
            .read()
            .await
            .get(&request.match_id)
            .copied()
            .ok_or_else(shrubbery_required)?;

        let view = request
            .view
            .ok_or_else(|| Status::invalid_argument("view is required"))?;

        if request.turn != view.turn {
            return Err(Status::invalid_argument(
                "request turn and view turn must match",
            ));
        }

        let to_act = Chapter::try_from(view.to_act)
            .map_err(|_| Status::invalid_argument("view has unknown chapter"))?;

        if to_act != info.chapter {
            return Err(Status::invalid_argument(
                "view is not for this bot's chapter",
            ));
        }

        let orders = choose_order(&view, info.chapter).into_iter().collect();

        Ok(Response::new(GetOrdersResponse {
            turn: request.turn,
            orders,
        }))
    }

    async fn match_ended(
        &self,
        _request: Request<MatchEndedRequest>,
    ) -> Result<Response<MatchEndedResponse>, Status> {
        Ok(Response::new(MatchEndedResponse {}))
    }

    async fn submit_replay(
        &self,
        _request: Request<SubmitReplayRequest>,
    ) -> Result<Response<SubmitReplayResponse>, Status> {
        Ok(Response::new(SubmitReplayResponse {}))
    }
}

fn shrubbery_required() -> Status {
    Status::failed_precondition(SHRUBBERY_REQUIRED)
}

fn coords(knight: &Knight) -> Option<(u32, u32)> {
    knight
        .position
        .as_ref()
        .map(|position| (position.x, position.y))
}

fn distance(a: (u32, u32), b: (u32, u32)) -> u32 {
    a.0.abs_diff(b.0) + a.1.abs_diff(b.1)
}

pub fn choose_order(view: &BattlefieldView, chapter: Chapter) -> Option<KnightOrder> {
    let opponent = match chapter {
        Chapter::A => Chapter::B,
        Chapter::B => Chapter::A,
        Chapter::Unspecified => return None,
    };

    let mut best_pair: Option<(&Knight, &Knight)> = None;

    for ally in view
        .knights
        .iter()
        .filter(|knight| knight.hp > 0 && Chapter::try_from(knight.chapter).ok() == Some(chapter))
    {
        let Some(ally_position) = coords(ally) else {
            continue;
        };

        for enemy in view.knights.iter().filter(|knight| {
            knight.hp > 0 && Chapter::try_from(knight.chapter).ok() == Some(opponent)
        }) {
            let Some(enemy_position) = coords(enemy) else {
                continue;
            };

            let candidate_key = (
                distance(ally_position, enemy_position),
                ally.unit_id.as_str(),
                enemy.unit_id.as_str(),
            );

            let is_better = match best_pair {
                None => true,
                Some((best_ally, best_enemy)) => {
                    let best_key = (
                        distance(
                            coords(best_ally).expect("selected ally has a position"),
                            coords(best_enemy).expect("selected enemy has a position"),
                        ),
                        best_ally.unit_id.as_str(),
                        best_enemy.unit_id.as_str(),
                    );
                    candidate_key < best_key
                }
            };

            if is_better {
                best_pair = Some((ally, enemy));
            }
        }
    }

    let (unit, target) = best_pair?;
    let start = coords(unit)?;
    let target_position = coords(target)?;

    let occupied: HashSet<_> = view
        .knights
        .iter()
        .filter(|knight| knight.hp > 0)
        .filter_map(coords)
        .collect();

    let mut destination = start;
    let mut best_key = (distance(start, target_position), 0, start.1, start.0);

    for y in 0..view.height {
        for x in 0..view.width {
            let candidate = (x, y);
            let movement = distance(start, candidate);

            if movement > unit.move_range {
                continue;
            }

            if candidate != start && occupied.contains(&candidate) {
                continue;
            }

            let key = (distance(candidate, target_position), movement, y, x);

            if key < best_key {
                destination = candidate;
                best_key = key;
            }
        }
    }

    let target_distance = distance(destination, target_position);

    let attack_target = (target_distance <= 1 && target_distance <= unit.attack_range)
        .then(|| target.unit_id.clone());

    let move_to = (destination != start).then_some(Position {
        x: destination.0,
        y: destination.1,
    });

    Some(KnightOrder {
        unit_id: unit.unit_id.clone(),
        move_to,
        attack_target,
    })
}

#[cfg(test)]
mod tets {
    use super::*;
    use ni_proto::ni::v1::{bot_service_server::BotService, BoardLayout, MatchEndedRequest, Rules};

    use tonic::Code;

    fn knight(id: &str, chapter: Chapter, x: u32, move_range: u32) -> Knight {
        Knight {
            unit_id: id.to_string(),
            chapter: chapter as i32,
            position: Some(Position { x, y: 0 }),
            hp: 10,
            move_range,
            attack_range: 2,
            attack_damage: 4,
        }
    }

    fn view(a_x: u32, b_x: u32) -> BattlefieldView {
        BattlefieldView {
            width: 8,
            height: 1,
            shrubbery: vec![],
            knights: vec![
                knight("A1", Chapter::A, a_x, 3),
                knight("B1", Chapter::B, b_x, 3),
            ],
            to_act: Chapter::A as i32,
            turn: 1,
            time_remaining_ms: 0,
        }
    }

    #[test]
    fn distant_knight_moves_deterministically_toward_enemy() {
        let view = view(0, 7);
        let first = choose_order(&view, Chapter::A).unwrap();
        let second = choose_order(&view, Chapter::A).unwrap();

        assert_eq!(first, second);
        assert_eq!(first.unit_id, "A1");
        assert_eq!(first.move_to, Some(Position { x: 3, y: 0 }));
        assert_eq!(first.attack_target, None);
    }

    #[test]
    fn adjacent_knight_attacks_without_moving() {
        let order = choose_order(&view(3, 4), Chapter::A).unwrap();

        assert_eq!(order.move_to, None);
        assert_eq!(order.attack_target.as_deref(), Some("B1"));
    }

    #[test]
    fn no_living_enemy_means_no_order() {
        let mut view = view(0, 7);
        view.knights[1].hp = 0;

        assert_eq!(choose_order(&view, Chapter::A), None);
    }

    #[tokio::test]
    async fn lifecycle_uses_shrubbery_required_for_unknown_matches() {
        let bot = ReferenceBot::default();

        let error = bot
            .get_orders(Request::new(GetOrdersRequest {
                match_id: "missing".to_string(),
                turn: 1,
                view: Some(view(0, 7)),
            }))
            .await
            .unwrap_err();

        assert_eq!(error.code(), Code::FailedPrecondition);
        assert_eq!(error.message(), SHRUBBERY_REQUIRED);

        bot.new_match(Request::new(NewMatchRequest {
            match_id: "known".to_string(),
            chapter: Chapter::A as i32,
            board: Some(BoardLayout {
                width: 8,
                height: 1,
                shrubbery: vec![],
            }),
            rules: Some(Rules::default()),
            turn: 0,
        }))
        .await
        .unwrap();

        bot.match_ended(Request::new(MatchEndedRequest {
            match_id: "known".to_string(),
            outcome: 0,
            reason: 0,
        }))
        .await
        .unwrap();
    }
}
