// The authoritative match loop: M3's policy, M4's instrumentation.

use std::{
    path::PathBuf,
    time::{Duration, Instant},
};

use crate::{
    convert::{battlefield_view, new_match_request, order_from_proto, resume_match_request},
    log::{MatchLog, TurnEvent},
    policy::{
        classify, describe, CallFailure, ForfeitReason, MatchConclusion, Strikes, TimeControl,
    },
    process::BotProcess,
    render::{render_board, render_forfeited_turn, render_result, render_turn},
};
use anyhow::{ensure, Result};
use ni_game::{Chapter, MatchState, MatchStatus, Order, Rules};
use ni_proto::{
    ni::v1::{
        GetOrdersRequest, GetOrdersResponse, IdentifyRequest, MatchEndedRequest, Replay,
        SubmitReplayRequest,
    },
    PROTOCOL_VERSION,
};
use prost::Message as _;
use tonic::{Request, Status};
use tracing::{field, info, info_span, warn, Instrument, Span};

const SETUP_DEADLINE: Duration = Duration::from_secs(5);
/// Deliberately generous: the replay is the largest message in the system,
/// and post 6 wants to measure it rather than time it out.
const REPLAY_DEADLINE: Duration = Duration::from_secs(10);
const SERVICE: &str = "ni.v1.BotService";

pub struct RunOptions {
    pub delay: Duration,
    pub quiet: bool,
    pub match_id: String,
    pub time: TimeControl,
    /// `None` disables the JSONL file; the replay is built either way.
    pub match_log: Option<PathBuf>,
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

/// What one turn's worth of asking cost, whatever the answer was.
struct CallReport {
    outcome: TurnOutcome,
    /// Wall-clock duration of the attempt that produced `outcome`.
    latency: Duration,
    /// 2 after a `SHRUBBERY_REQUIRED` recovery, 1 otherwise.
    attempts: u32,
}

/// One span per outgoing gRPC call, named and tagged the way a tracing
/// backend expects. `otel.*` fields are read by `tracing-opentelemetry`;
/// `rpc.*` are the OpenTelemetry semantic conventions for RPC clients.
///
/// Every field a caller might `record` later must be declared here as
/// `field::Empty`: a `tracing` span's field *set* is fixed at creation, and
/// recording an undeclared field is silently dropped.
fn client_span(method: &'static str) -> Span {
    let span = info_span!(
        "grpc",
        otel.name = field::Empty,
        otel.kind = "client",
        otel.status_code = field::Empty,
        rpc.system = "grpc",
        rpc.service = SERVICE,
        rpc.method = method,
        rpc.grpc.status_code = field::Empty,
        ni.turn = field::Empty,
        ni.latency_us = field::Empty,
        ni.replay_bytes = field::Empty,
    );

    span.record("otel.name", format!("{SERVICE}/{method}").as_str());
    span
}

/// Record how a call ended on its own span, so a failure is visible in the
/// dashboard without reading any logs.
fn record_status(span: &Span, result: &Result<impl Sized, Status>) {
    match result {
        Ok(_) => {
            span.record("rpc.grpc.status_code", tonic::Code::Ok as i32);
            span.record("otel.status_code", "OK");
        }
        Err(status) => {
            span.record("rpc.grpc.status_code", status.code() as i32);
            span.record("otel.status_code", "ERROR");
        }
    }
}

async fn new_match(
    bot: &mut BotProcess,
    state: &MatchState,
    chapter: Chapter,
    options: &RunOptions,
) -> Result<()> {
    let span = client_span("NewMatch");

    async {
        let mut request = Request::new(new_match_request(
            state,
            &options.match_id,
            chapter,
            options.time,
        ));
        request.set_timeout(SETUP_DEADLINE);
        ni_telemetry::inject_context(&mut request);

        let result = bot.client.new_match(request).await;
        record_status(&Span::current(), &result);
        result?;

        info!(chapter = ?chapter, "match announced");
        Ok(())
    }
    .instrument(span)
    .await
}

#[tracing::instrument(
    name = "match",
    skip_all,
    fields(otel.name = "match", ni.match_id = %options.match_id)
)]
pub async fn run_match(
    bot_a: &mut BotProcess,
    bot_b: &mut BotProcess,
    options: RunOptions,
) -> Result<MatchConclusion> {
    identify(bot_a, "A").await?;
    identify(bot_b, "B").await?;

    let mut state = ni_game::standard_match(Rules::standard());

    let mut log = MatchLog::create(
        &options.match_id,
        &state,
        options.time,
        options.match_log.as_deref(),
    )?;

    new_match(bot_a, &state, Chapter::A, &options).await?;
    new_match(bot_b, &state, Chapter::B, &options).await?;

    info!(
        match_id = %options.match_id,
        turn_deadline_ms = options.time.turn_deadline_ms(),
        strike_limit = options.time.strike_limit,
        "match begins"
    );

    if !options.quiet {
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

        // One span per turn, so a whole turn — including a recovery and its
        // retry — is a single subtree in the trace.
        let turn_span = info_span!(
            "turn",
            otel.name = "turn",
            ni.turn = turn,
            ni.chapter = chapter_name(acting),
        );

        let report = request_orders(bot, &state, acting, &options)
            .instrument(turn_span.clone())
            .await;

        let CallReport {
            outcome,
            latency,
            attempts,
        } = report;

        match outcome {
            TurnOutcome::Orders(orders) => {
                strikes.clear();

                let (next_state, outcomes) = ni_game::apply_orders(state, acting, &orders);
                state = next_state;

                turn_span.in_scope(|| {
                    info!(
                        turn,
                        chapter = chapter_name(acting),
                        orders = outcomes.len(),
                        latency_us = latency.as_micros(),
                        "turn resolved"
                    );

                    log.record_turn(TurnEvent {
                        turn,
                        acting,
                        latency,
                        status: "ok",
                        detail: None,
                        outcomes: &outcomes,
                        strikes: 0,
                        attempts,
                    });
                });

                if !options.quiet {
                    print!("{}", render_turn(&state, acting, turn, &outcomes));
                }
            }
            TurnOutcome::TurnForfeited { reason, detail } => {
                strikes.record(reason);
                let strikes = *strikes;

                turn_span.in_scope(|| {
                    warn!(
                        turn,
                        chapter = chapter_name(acting),
                        reason = ?reason,
                        detail = %detail,
                        strike = strikes.count(),
                        limit = options.time.strike_limit,
                        "turn forfeited"
                    );

                    log.record_turn(TurnEvent {
                        turn,
                        acting,
                        latency,
                        status: log_status(reason),
                        detail: Some(&detail),
                        outcomes: &[],
                        strikes: strikes.count(),
                        attempts,
                    });
                });

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
                turn_span.in_scope(|| {
                    warn!(
                        turn,
                        chapter = chapter_name(acting),
                        reason = ?reason,
                        detail = %detail,
                        "chapter cannot continue"
                    );

                    log.record_turn(TurnEvent {
                        turn,
                        acting,
                        latency,
                        status: log_status(reason),
                        detail: Some(&detail),
                        outcomes: &[],
                        strikes: strikes.count(),
                        attempts,
                    });
                });

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

    log.record_result(conclusion);

    info!(
        winner = ?conclusion.winner(),
        turns = state.turn,
        "match ended"
    );

    notify_match_ended(bot_a, &options.match_id, Chapter::A, conclusion).await;
    notify_match_ended(bot_b, &options.match_id, Chapter::B, conclusion).await;

    let replay = log.replay(conclusion);
    submit_replay(bot_a, Chapter::A, &replay).await;
    submit_replay(bot_b, Chapter::B, &replay).await;

    println!("{}", render_result(conclusion));
    Ok(conclusion)
}

async fn request_orders(
    bot: &mut BotProcess,
    state: &MatchState,
    acting: Chapter,
    options: &RunOptions,
) -> CallReport {
    let (result, latency) = call_get_orders(bot, state, acting, options).await;

    let status = match result {
        Ok(response) => {
            return CallReport {
                outcome: orders_or_protocol_error(response, state.turn),
                latency,
                attempts: 1,
            }
        }
        Err(status) => status,
    };

    let outcome = match classify(&status) {
        CallFailure::Timeout => TurnOutcome::TurnForfeited {
            reason: ForfeitReason::Timeout,
            detail: format!(
                "no answer within {}ms ({})",
                options.time.turn_deadline_ms(),
                describe(&status)
            ),
        },
        CallFailure::NeedsShrubbery => {
            return recover_and_retry(bot, state, acting, options).await;
        }
        CallFailure::Unreachable(detail) => TurnOutcome::MatchForfeited {
            reason: ForfeitReason::Crash,
            detail: with_exit_status(bot, detail),
        },
        CallFailure::Protocol(detail) => TurnOutcome::TurnForfeited {
            reason: ForfeitReason::Protocol,
            detail,
        },
    };

    CallReport {
        outcome,
        latency,
        attempts: 1,
    }
}

fn with_exit_status(bot: &mut BotProcess, detail: String) -> String {
    match bot.exit_status() {
        Ok(Some(code)) => format!("{detail} (process exited with code {code})"),
        _ => format!("{detail} (process still running)"),
    }
}

async fn recover_and_retry(
    bot: &mut BotProcess,
    state: &MatchState,
    acting: Chapter,
    options: &RunOptions,
) -> CallReport {
    warn!(
        turn = state.turn,
        chapter = chapter_name(acting),
        "bot has not been brought a shrubbery; re-sending NewMatch and retrying"
    );

    let resume = client_span("NewMatch");
    let resume_result = async {
        let mut request = Request::new(resume_match_request(
            state,
            &options.match_id,
            acting,
            options.time,
        ));
        request.set_timeout(SETUP_DEADLINE);
        ni_telemetry::inject_context(&mut request);

        let result = bot.client.new_match(request).await;
        record_status(&Span::current(), &result);
        result
    }
    .instrument(resume)
    .await;

    if let Err(status) = resume_result {
        let outcome = match classify(&status) {
            CallFailure::Unreachable(detail) => TurnOutcome::MatchForfeited {
                reason: ForfeitReason::Crash,
                detail: with_exit_status(bot, detail),
            },
            _ => TurnOutcome::MatchForfeited {
                reason: ForfeitReason::Protocol,
                detail: format!("rejected the replacement NewMatch: {}", describe(&status)),
            },
        };

        return CallReport {
            outcome,
            latency: Duration::ZERO,
            attempts: 2,
        };
    }

    let (result, latency) = call_get_orders(bot, state, acting, options).await;

    let status = match result {
        Ok(response) => {
            return CallReport {
                outcome: orders_or_protocol_error(response, state.turn),
                latency,
                attempts: 2,
            }
        }
        Err(status) => status,
    };

    let outcome = match classify(&status) {
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
    };

    CallReport {
        outcome,
        latency,
        attempts: 2,
    }
}

fn orders_or_protocol_error(response: GetOrdersResponse, turn: u32) -> TurnOutcome {
    if response.turn != turn {
        return TurnOutcome::TurnForfeited {
            reason: ForfeitReason::Protocol,
            detail: format!(
                "echoed turn {} while the engine plays {turn}",
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
) -> (Result<GetOrdersResponse, Status>, Duration) {
    let span = client_span("GetOrders");
    span.record("ni.turn", state.turn);

    async {
        let mut request = Request::new(GetOrdersRequest {
            match_id: options.match_id.clone(),
            turn: state.turn,
            view: Some(battlefield_view(state)),
        });
        request.set_timeout(options.time.turn_deadline);

        // The one line that makes the bot's spans children of this one.
        // It must run inside the span it is describing.
        ni_telemetry::inject_context(&mut request);

        let started = Instant::now();
        let result = bot.client.get_orders(request).await;
        let latency = started.elapsed();

        record_status(&Span::current(), &result);
        Span::current().record("ni.latency_us", latency.as_micros() as i64);

        match &result {
            Ok(_) => info!(
                turn = state.turn,
                chapter = chapter_name(acting),
                latency_us = latency.as_micros(),
                "GetOrders answered"
            ),
            Err(status) => warn!(
                turn = state.turn,
                chapter = chapter_name(acting),
                latency_us = latency.as_micros(),
                code = ?status.code(),
                "GetOrders failed"
            ),
        }

        (result.map(|response| response.into_inner()), latency)
    }
    .instrument(span)
    .await
}

async fn identify(bot: &mut BotProcess, label: &str) -> Result<()> {
    let span = client_span("Identify");

    let identity = async {
        let mut request = Request::new(IdentifyRequest {
            protocol_version: PROTOCOL_VERSION,
        });
        request.set_timeout(SETUP_DEADLINE);
        ni_telemetry::inject_context(&mut request);

        let result = bot.client.identify(request).await;
        record_status(&Span::current(), &result);
        result
    }
    .instrument(span)
    .await?
    .into_inner();

    ensure!(
        identity.protocol_version == PROTOCOL_VERSION,
        "bot {label} ({}) speaks protocol {}, engine requires {}",
        identity.name,
        identity.protocol_version,
        PROTOCOL_VERSION
    );

    info!(
        bot = label,
        pid = bot.id(),
        endpoint = bot.endpoint(),
        name = identity.name,
        version = identity.version,
        protocol = identity.protocol_version,
        "bot identified"
    );

    Ok(())
}

async fn notify_match_ended(
    bot: &mut BotProcess,
    match_id: &str,
    chapter: Chapter,
    conclusion: MatchConclusion,
) {
    let span = client_span("MatchEnded");

    async {
        let mut request = Request::new(MatchEndedRequest {
            match_id: match_id.to_string(),
            outcome: conclusion.outcome_for(chapter),
            reason: conclusion.end_reason(),
        });
        request.set_timeout(SETUP_DEADLINE);
        ni_telemetry::inject_context(&mut request);

        let result = bot.client.match_ended(request).await;
        record_status(&Span::current(), &result);

        // Best effort by definition: the most likely reason a match ended is
        // that this bot stopped answering.
        if let Err(status) = result {
            warn!(chapter = ?chapter, code = ?status.code(), "could not deliver MatchEnded");
        }
    }
    .instrument(span)
    .await
}

async fn submit_replay(bot: &mut BotProcess, chapter: Chapter, replay: &Replay) {
    let span = client_span("SubmitReplay");

    async {
        let bytes = replay.encoded_len();

        let mut request = Request::new(SubmitReplayRequest {
            replay: Some(replay.clone()),
        });
        request.set_timeout(REPLAY_DEADLINE);
        ni_telemetry::inject_context(&mut request);

        Span::current().record("ni.replay_bytes", bytes as i64);

        let result = bot.client.submit_replay(request).await;
        record_status(&Span::current(), &result);

        match result {
            Ok(_) => info!(
                chapter = ?chapter,
                turns = replay.turns.len(),
                encoded_bytes = bytes,
                "replay accepted"
            ),
            Err(status) => warn!(
                chapter = ?chapter,
                code = ?status.code(),
                "could not deliver the replay"
            ),
        }
    }
    .instrument(span)
    .await
}

fn chapter_name(chapter: Chapter) -> &'static str {
    match chapter {
        Chapter::A => "A",
        Chapter::B => "B",
    }
}

/// The forfeit taxonomy, spelled the way the JSONL log spells it.
fn log_status(reason: ForfeitReason) -> &'static str {
    match reason {
        ForfeitReason::Timeout => "timeout",
        ForfeitReason::Crash => "unreachable",
        ForfeitReason::Protocol => "protocol",
    }
}
