// The authoritative M2 match loop

use std::time::{Duration, Instant};

use crate::{
    convert::{battlefield_view, new_match_request, order_from_proto, resume_match_request},
    policy::{
        classify, describe, CallFailure, ForfeitReason, MatchConclusion, Strikes, TimeControl,
    },
    process::BotProcess,
    render::{render_board, render_forfeited_turn, render_result, render_turn},
};
use anyhow::{ensure, Result};
use ni_game::{Chapter, MatchState, MatchStatus, Order, Rules};
use ni_proto::{
    ni::v1::{GetOrdersRequest, GetOrdersResponse, IdentifyRequest, MatchEndedRequest},
    PROTOCOL_VERSION,
};
use tonic::{Request, Status};

const SETUP_DEADLINE: Duration = Duration::from_secs(5);

pub struct RunOptions {
    pub delay: Duration,
    pub quiet: bool,
    pub match_id: String,
    pub time: TimeControl,
}

enum TurnOutcome {
    Orders(Vec<Order>),
    TurnForfeited {
        reason: ForfeitReason,
        detail: String,
    },
    MatchForfeited {
        reason: ForfeitReason,
        detail: String,
    },
}

async fn new_match(
    bot: &mut BotProcess,
    state: &MatchState,
    chapter: Chapter,
    options: &RunOptions,
) -> Result<()> {
    let mut request = Request::new(new_match_request(
        state,
        &options.match_id,
        chapter,
        options.time,
    ));
    request.set_timeout(SETUP_DEADLINE);

    bot.client.new_match(request).await?;
    Ok(())
}

pub async fn run_match(
    bot_a: &mut BotProcess,
    bot_b: &mut BotProcess,
    options: RunOptions,
) -> Result<MatchConclusion> {
    identify(bot_a, "A").await?;
    identify(bot_b, "B").await?;

    let mut state = ni_game::standard_match(Rules::standard());

    new_match(bot_a, &state, Chapter::A, &options).await?;
    new_match(bot_b, &state, Chapter::B, &options).await?;

    if !options.quiet {
        println!(
            "match {} begins (deadline {}ms, {} strikes)",
            options.match_id,
            options.time.turn_deadline_ms(),
            options.time.strike_limit
        );
        print!("{}", render_board(&state));
    }

    let mut strikes_a = Strikes::default();
    let mut strikes_b = Strikes::default();

    let conclusion = loop {
        let acting = state.to_act;
        let turn = state.turn;

        let (bot, strikes) = match acting {
            Chapter::A => (&mut *bot_a, &mut strikes_a),
            Chapter::B => (&mut *bot_b, &mut strikes_b),
        };

        match request_orders(bot, &state, acting, &options).await {
            TurnOutcome::Orders(orders) => {
                strikes.clear();

                let (next_state, outcomes) = ni_game::apply_orders(state, acting, &orders);
                state = next_state;

                if !options.quiet {
                    print!("{}", render_turn(&state, acting, turn, &outcomes));
                }
            }
            TurnOutcome::TurnForfeited { reason, detail } => {
                strikes.record(reason);
                let strikes = *strikes;

                if !options.quiet {
                    print!(
                        "{}",
                        render_forfeited_turn(
                            turn,
                            acting,
                            reason,
                            &detail,
                            strikes,
                            options.time.strike_limit
                        )
                    );
                }

                if let Some(reason) = strikes.exhausted(options.time.strike_limit) {
                    break MatchConclusion::Forfeit {
                        loser: acting,
                        reason,
                    };
                }
            }
            TurnOutcome::MatchForfeited { reason, detail } => {
                if !options.quiet {
                    println!("turn {turn}: chapter {acting:?} cannot continue ({detail})");
                }

                break MatchConclusion::Forfeit {
                    loser: acting,
                    reason,
                };
            }
        }

        match ni_game::match_status(&state) {
            MatchStatus::InProgress => {}
            finished => break MatchConclusion::Decided(finished),
        }

        ni_game::end_turn(&mut state);

        match ni_game::match_status(&state) {
            MatchStatus::InProgress => {}
            finished => break MatchConclusion::Decided(finished),
        }

        if !options.delay.is_zero() {
            tokio::time::sleep(options.delay).await;
        }
    };

    notify_match_ended(bot_a, &options.match_id, Chapter::A, conclusion).await;
    notify_match_ended(bot_b, &options.match_id, Chapter::B, conclusion).await;

    println!("{}", render_result(conclusion));
    Ok(conclusion)
}

async fn request_orders(
    bot: &mut BotProcess,
    state: &MatchState,
    acting: Chapter,
    options: &RunOptions,
) -> TurnOutcome {
    let status = match call_get_orders(bot, state, acting, options).await {
        Ok(response) => return orders_or_protocol_error(response, state.turn),
        Err(status) => status,
    };

    match classify(&status) {
        CallFailure::Timeout => TurnOutcome::TurnForfeited {
            reason: ForfeitReason::Timeout,
            detail: format!(
                "no answer within {}ms ({})",
                options.time.turn_deadline_ms(),
                describe(&status)
            ),
        },
        CallFailure::NeedsShrubbery => recover_and_retry(bot, state, acting, options).await,
        CallFailure::Unreachable(detail) => TurnOutcome::MatchForfeited {
            reason: ForfeitReason::Crash,
            detail: with_exit_status(bot, detail),
        },
        CallFailure::Protocol(detail) => TurnOutcome::TurnForfeited {
            reason: ForfeitReason::Protocol,
            detail,
        },
    }
}

fn with_exit_status(bot: &mut BotProcess, detail: String) -> String {
    match bot.exit_status() {
        Ok(Some(code)) => format!("{detail} (process exited with code {code})"),
        _ => format!("{detail} (process still running"),
    }
}

async fn recover_and_retry(
    bot: &mut BotProcess,
    state: &MatchState,
    acting: Chapter,
    options: &RunOptions,
) -> TurnOutcome {
    if !options.quiet {
        println!(
            "turn {}: chapter {acting:?} has not been brought a shrubbery; \
            re-sending NewMatch and retrying",
            state.turn
        )
    };

    let mut request = Request::new(resume_match_request(
        state,
        &options.match_id,
        acting,
        options.time,
    ));
    request.set_timeout(SETUP_DEADLINE);

    if let Err(status) = bot.client.new_match(request).await {
        return match classify(&status) {
            CallFailure::Unreachable(detail) => TurnOutcome::MatchForfeited {
                reason: ForfeitReason::Crash,
                detail: with_exit_status(bot, detail),
            },
            _ => TurnOutcome::MatchForfeited {
                reason: ForfeitReason::Protocol,
                detail: format!("rejected the replacement NewMatch: {}", describe(&status)),
            },
        };
    }

    let status = match call_get_orders(bot, state, acting, options).await {
        Ok(response) => return orders_or_protocol_error(response, state.turn),
        Err(status) => status,
    };

    match classify(&status) {
        CallFailure::Timeout => TurnOutcome::TurnForfeited {
            reason: ForfeitReason::Timeout,
            detail: format!("no answer after recovery ({})", describe(&status)),
        },
        CallFailure::Unreachable(detail) => TurnOutcome::MatchForfeited {
            reason: ForfeitReason::Crash,
            detail: with_exit_status(bot, detail),
        },
        CallFailure::NeedsShrubbery => TurnOutcome::MatchForfeited {
            reason: ForfeitReason::Protocol,
            detail: "demanded a shrubbery again after NewMatch".to_string(),
        },
        CallFailure::Protocol(detail) => TurnOutcome::TurnForfeited {
            reason: ForfeitReason::Protocol,
            detail,
        },
    }
}

fn orders_or_protocol_error(response: GetOrdersResponse, turn: u32) -> TurnOutcome {
    if response.turn != turn {
        return TurnOutcome::TurnForfeited {
            reason: ForfeitReason::Protocol,
            detail: format!(
                "echoed turn {} while the engine playes {turn}",
                response.turn
            ),
        };
    }

    TurnOutcome::Orders(response.orders.into_iter().map(order_from_proto).collect())
}

async fn call_get_orders(
    bot: &mut BotProcess,
    state: &MatchState,
    acting: Chapter,
    options: &RunOptions,
) -> Result<GetOrdersResponse, Status> {
    let mut request = Request::new(GetOrdersRequest {
        match_id: options.match_id.clone(),
        turn: state.turn,
        view: Some(battlefield_view(state)),
    });
    request.set_timeout(options.time.turn_deadline);

    let started = Instant::now();
    let result = bot.client.get_orders(request).await;
    let elapsed = started.elapsed();

    if !options.quiet {
        println!(
            "turn {}: GetOrders -> chapter {acting:?} {} in {}ms",
            state.turn,
            match &result {
                Ok(_) => "answered".to_string(),
                Err(status) => format!("failed with {:?}", status.code()),
            },
            elapsed.as_millis()
        );
    }

    result.map(|response| response.into_inner())
}

async fn identify(bot: &mut BotProcess, label: &str) -> Result<()> {
    let mut request = Request::new(IdentifyRequest {
        protocol_version: PROTOCOL_VERSION,
    });
    request.set_timeout(SETUP_DEADLINE);

    let identity = bot.client.identify(request).await?.into_inner();

    ensure!(
        identity.protocol_version == PROTOCOL_VERSION,
        "bot {label} ({}) speaks protocol {}, engine requires {}",
        identity.name,
        identity.protocol_version,
        PROTOCOL_VERSION
    );

    println!(
        "bot {label}: pid={}, tcp://{}, {} {} (protocol {})",
        bot.id()
            .map(|id| id.to_string())
            .unwrap_or_else(|| "unknown".to_string()),
        bot.endpoint(),
        identity.name,
        identity.version,
        identity.protocol_version,
    );

    Ok(())
}

async fn notify_match_ended(
    bot: &mut BotProcess,
    match_id: &str,
    chapter: Chapter,
    conclusion: MatchConclusion,
) {
    let mut request = Request::new(MatchEndedRequest {
        match_id: match_id.to_string(),
        outcome: conclusion.outcome_for(chapter),
        reason: conclusion.end_reason(),
    });
    request.set_timeout(SETUP_DEADLINE);

    // Best effort by definition: the most likely reason a match ended is
    // that this bot stopped answering.
    if let Err(error) = bot.client.match_ended(request).await {
        eprintln!("could not notify bot {chapter:?} that the match ended: {error}");
    }
}
