//! `roger-the-shrubber`: a bot that misbehaves on purpose.
//!
//! Every failure the M3 engine claims to survive needs something that
//! actually causes it. Roger is that something: he is slow, he forgets
//! matches, he orders knights he does not own, he answers about the wrong
//! turn, and — if asked — he dies mid-call.
//!
//! He is a *test fixture with a socket*. The strategy itself is borrowed
//! from the reference bot, so a Roger with no flags plays a normal match.

use std::{
    collections::HashMap,
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

use clap::Args;
use ni_bot::choose_order;
use ni_proto::{
    ni::v1::{
        bot_service_server::BotService, BattlefieldView, Chapter, GetOrdersRequest,
        GetOrdersResponse, IdentifyRequest, IdentifyResponse, KnightOrder, MatchEndedRequest,
        MatchEndedResponse, NewMatchRequest, NewMatchResponse, Position, SubmitReplayRequest,
        SubmitReplayResponse,
    },
    PROTOCOL_VERSION, SHRUBBERY_REQUIRED,
};
use tokio::sync::RwLock;
use tonic::{Request, Response, Status};

/// Which misbehaviours are switched on. Everything is off by default, so
/// `roger-the-shrubber` with no flags is a (slightly dim) normal opponent.
#[derive(Args, Clone, Copy, Debug, Default)]
pub struct Mischief {
    /// Think this long before answering `GetOrders`.
    #[arg(long, default_value_t = 0)]
    pub sleep_ms: u64,

    /// Restrict `--sleep-ms` to this turn only. Without it, every turn is
    /// slow — which is how you reach the strike limit.
    #[arg(long)]
    pub sleep_on_turn: Option<u32>,

    /// Exit the process (uncleanly) when this turn arrives.
    #[arg(long)]
    pub crash_on_turn: Option<u32>,

    /// Forget every match once, when this turn arrives: the next `GetOrders`
    /// answers `SHRUBBERY_REQUIRED`, like a restarted daemon would.
    #[arg(long)]
    pub forget_on_turn: Option<u32>,

    /// Order a knight to a tile far off the board.
    #[arg(long)]
    pub illegal_orders: bool,

    /// Order the *opponent's* knights around.
    #[arg(long)]
    pub steal_knights: bool,

    /// Echo the wrong turn number in the response.
    #[arg(long)]
    pub wrong_turn: bool,
}

pub struct Roger {
    mischief: Mischief,
    matches: RwLock<HashMap<String, Chapter>>,
    /// `forget_on_turn` fires once. Without this latch the bot would forget
    /// the match again on the retry, and recovery could never converge.
    already_forgot: AtomicBool,
}

impl Roger {
    pub fn new(mischief: Mischief) -> Self {
        Self {
            mischief,
            matches: RwLock::new(HashMap::new()),
            already_forgot: AtomicBool::new(false),
        }
    }

    async fn maybe_forget(&self, turn: u32) {
        if self.mischief.forget_on_turn != Some(turn) {
            return;
        }

        if self.already_forgot.swap(true, Ordering::SeqCst) {
            return;
        }

        eprintln!("roger: forgetting every match at turn {turn}");
        self.matches.write().await.clear();
    }

    async fn maybe_sleep(&self, turn: u32) {
        let wanted = match self.mischief.sleep_on_turn {
            Some(target) => target == turn,
            None => self.mischief.sleep_ms > 0,
        };

        if wanted && self.mischief.sleep_ms > 0 {
            eprintln!(
                "roger: sleeping {}ms on turn {turn}",
                self.mischief.sleep_ms
            );
            tokio::time::sleep(Duration::from_millis(self.mischief.sleep_ms)).await;
        }
    }

    fn maybe_crash(&self, turn: u32) {
        if self.mischief.crash_on_turn == Some(turn) {
            eprint!("roger: dying on turn {turn}");
            // Not a panic: a panic in a handler is caught and turned into a
            // status. This kills the process mid-request, which is what a
            // real crash looks like from the engine's side of the socket.
            std::process::exit(101);
        }
    }
}

#[tonic::async_trait]
impl BotService for Roger {
    async fn identify(
        &self,
        _request: Request<IdentifyRequest>,
    ) -> Result<Response<IdentifyResponse>, Status> {
        Ok(Response::new(IdentifyResponse {
            name: "roger-the-shrubber".to_string(),
            version: env!("CARGO_PKG_VERSION").to_string(),
            protocol_version: PROTOCOL_VERSION,
        }))
    }

    async fn new_match(
        &self,
        request: Request<NewMatchRequest>,
    ) -> Result<Response<NewMatchResponse>, Status> {
        let request = request.into_inner();

        let chapter = Chapter::try_from(request.chapter)
            .map_err(|_| Status::invalid_argument("unknown chapter"))?;

        eprintln!(
            "roger: NewMatch {} as {chapter:?} at turn {}",
            request.match_id, request.turn
        );

        self.matches.write().await.insert(request.match_id, chapter);

        Ok(Response::new(NewMatchResponse {}))
    }

    async fn get_orders(
        &self,
        request: Request<GetOrdersRequest>,
    ) -> Result<Response<GetOrdersResponse>, Status> {
        let request = request.into_inner();

        self.maybe_crash(request.turn);
        self.maybe_forget(request.turn).await;

        let chapter = self
            .matches
            .read()
            .await
            .get(&request.match_id)
            .copied()
            .ok_or_else(|| Status::failed_precondition(SHRUBBERY_REQUIRED))?;

        let view = request
            .view
            .ok_or_else(|| Status::invalid_argument("view is required"))?;

        self.maybe_sleep(request.turn).await;

        let turn = if self.mischief.wrong_turn {
            request.turn.wrapping_add(1)
        } else {
            request.turn
        };

        Ok(Response::new(GetOrdersResponse {
            turn,
            orders: self.orders(&view, chapter),
        }))
    }

    async fn match_ended(
        &self,
        request: Request<MatchEndedRequest>,
    ) -> Result<Response<MatchEndedResponse>, Status> {
        let request = request.into_inner();
        eprintln!(
            "roger: MatchEnded {} outcome={} reason={}",
            request.match_id, request.outcome, request.reason
        );

        self.matches.write().await.remove(&request.match_id);
        Ok(Response::new(MatchEndedResponse {}))
    }

    async fn submit_replay(
        &self,
        _request: Request<SubmitReplayRequest>,
    ) -> Result<Response<SubmitReplayResponse>, Status> {
        Ok(Response::new(SubmitReplayResponse {}))
    }
}

impl Roger {
    fn orders(&self, view: &BattlefieldView, chapter: Chapter) -> Vec<KnightOrder> {
        if self.mischief.steal_knights {
            if let Some(enemy) = first_living(view, opponent(chapter)) {
                return vec![KnightOrder {
                    unit_id: enemy,
                    move_to: Some(Position { x: 0, y: 0 }),
                    attack_target: None,
                }];
            }
        }

        if self.mischief.illegal_orders {
            if let Some(ally) = first_living(view, chapter) {
                return vec![KnightOrder {
                    unit_id: ally,
                    move_to: Some(Position { x: 999, y: 999 }),
                    attack_target: None,
                }];
            }
        }

        choose_order(view, chapter).into_iter().collect()
    }
}

fn opponent(chapter: Chapter) -> Chapter {
    match chapter {
        Chapter::A => Chapter::B,
        Chapter::B => Chapter::A,
        Chapter::Unspecified => Chapter::Unspecified,
    }
}

fn first_living(view: &BattlefieldView, chapter: Chapter) -> Option<String> {
    view.knights
        .iter()
        .find(|knight| knight.hp > 0 && Chapter::try_from(knight.chapter).ok() == Some(chapter))
        .map(|knight| knight.unit_id.clone())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ni_proto::ni::v1::{BoardLayout, Knight, Rules};
    use tonic::Code;

    fn view(turn: u32) -> BattlefieldView {
        BattlefieldView {
            width: 8,
            height: 1,
            shrubbery: vec![],
            knights: vec![
                Knight {
                    unit_id: "A1".to_string(),
                    chapter: Chapter::A as i32,
                    position: Some(Position { x: 0, y: 0 }),
                    hp: 10,
                    move_range: 3,
                    attack_range: 2,
                    attack_damage: 4,
                },
                Knight {
                    unit_id: "B1".to_string(),
                    chapter: Chapter::B as i32,
                    position: Some(Position { x: 7, y: 0 }),
                    hp: 10,
                    move_range: 3,
                    attack_range: 2,
                    attack_damage: 4,
                },
            ],
            to_act: Chapter::A as i32,
            turn,
            time_remaining_ms: 0,
        }
    }

    async fn started(mischief: Mischief) -> Roger {
        let roger = Roger::new(mischief);

        roger
            .new_match(Request::new(NewMatchRequest {
                match_id: "m3-test".to_string(),
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

        roger
    }

    async fn ask(roger: &Roger, turn: u32) -> Result<GetOrdersResponse, Status> {
        roger
            .get_orders(Request::new(GetOrdersRequest {
                match_id: "m3-test".to_string(),
                turn,
                view: Some(view(turn)),
            }))
            .await
            .map(|response| response.into_inner())
    }

    #[tokio::test]
    async fn a_flagless_roger_plays_normally() {
        let roger = started(Mischief::default()).await;
        let response = ask(&roger, 1).await.unwrap();

        assert_eq!(response.turn, 1);
        assert_eq!(response.orders.len(), 1);
        assert_eq!(response.orders[0].unit_id, "A1");
    }

    #[tokio::test]
    async fn forgetting_happens_once_so_recovery_can_converge() {
        let roger = started(Mischief {
            forget_on_turn: Some(2),
            ..Mischief::default()
        })
        .await;

        assert!(ask(&roger, 1).await.is_ok());

        let error = ask(&roger, 2).await.unwrap_err();
        assert_eq!(error.code(), Code::FailedPrecondition);
        assert_eq!(error.message(), SHRUBBERY_REQUIRED);

        // The engine's answer to that status: re-send NewMatch, then retry
        // the same turn.
        started_again(&roger).await;
        assert!(ask(&roger, 2).await.is_ok());
    }

    async fn started_again(roger: &Roger) {
        roger
            .new_match(Request::new(NewMatchRequest {
                match_id: "m3-test".to_string(),
                chapter: Chapter::A as i32,
                board: Some(BoardLayout {
                    width: 8,
                    height: 1,
                    shrubbery: vec![],
                }),
                rules: Some(Rules::default()),
                turn: 2,
            }))
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn stolen_and_illegal_orders_are_still_well_formed_messages() {
        let thief = started(Mischief {
            steal_knights: true,
            ..Mischief::default()
        })
        .await;
        assert_eq!(ask(&thief, 1).await.unwrap().orders[0].unit_id, "B1");

        let fool = started(Mischief {
            illegal_orders: true,
            ..Mischief::default()
        })
        .await;
        let order = ask(&fool, 1).await.unwrap().orders.remove(0);
        assert_eq!(order.unit_id, "A1");
        assert_eq!(order.move_to, Some(Position { x: 999, y: 999 }));
    }

    #[tokio::test]
    async fn the_wrong_turn_flag_breaks_the_echo() {
        let roger = started(Mischief {
            wrong_turn: true,
            ..Mischief::default()
        })
        .await;

        assert_eq!(ask(&roger, 5).await.unwrap().turn, 6);
    }
}
