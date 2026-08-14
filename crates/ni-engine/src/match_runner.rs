// The authoritative M2 match loop

use std::time::Duration;

use crate::{
    convert::{battlefield_view, new_match_request, order_from_proto},
    process::BotProcess,
    render::{render_board, render_result, render_turn},
};
use anyhow::{ensure, Result};
use ni_game::{Chapter, EndReason, MatchStatus, Rules};
use ni_proto::{
    ni::v1::{GetOrdersRequest, IdentifyRequest, MatchEndReason, MatchEndedRequest, MatchOutcome},
    PROTOCOL_VERSION,
};

pub struct RunOptions {
    pub delay: Duration,
    pub quiet: bool,
    pub match_id: String,
}

pub async fn run_match(
    bot_a: &mut BotProcess,
    bot_b: &mut BotProcess,
    options: RunOptions,
) -> Result<MatchStatus> {
    identify(bot_a, "A").await?;
    identify(bot_b, "B").await?;

    let mut state = ni_game::standard_match(Rules::standard());

    bot_a
        .client
        .new_match(new_match_request(&state, &options.match_id, Chapter::A))
        .await?;

    bot_b
        .client
        .new_match(new_match_request(&state, &options.match_id, Chapter::B))
        .await?;

    if !options.quiet {
        println!("match {} begins", options.match_id);
        print!("{}", render_board(&state));
    }

    let final_status = loop {
        let acting = state.to_act;
        let turn = state.turn;

        let request = GetOrdersRequest {
            match_id: options.match_id.clone(),
            turn,
            view: Some(battlefield_view(&state)),
        };

        let response = match acting {
            Chapter::A => bot_a.client.get_orders(request).await?,
            Chapter::B => bot_b.client.get_orders(request).await?,
        }
        .into_inner();

        ensure!(
            response.turn == turn,
            "reponse turn {} does not match request turn {}",
            response.turn,
            turn
        );

        let orders = response
            .orders
            .into_iter()
            .map(order_from_proto)
            .collect::<Vec<_>>();

        let (next_state, outcomes) = ni_game::apply_orders(state, acting, &orders);
        state = next_state;

        if !options.quiet {
            print!("{}", render_turn(&state, acting, turn, &outcomes));
        }

        match ni_game::match_status(&state) {
            MatchStatus::InProgress => {}
            finished => break finished,
        }

        ni_game::end_turn(&mut state);

        match ni_game::match_status(&state) {
            MatchStatus::InProgress => {}
            finished => break finished,
        }

        if !options.delay.is_zero() {
            tokio::time::sleep(options.delay).await;
        }
    };

    notify_match_ended(bot_a, &options.match_id, Chapter::A, final_status).await;

    notify_match_ended(bot_b, &options.match_id, Chapter::B, final_status).await;

    println!("{}", render_result(final_status));
    Ok(final_status)
}

async fn identify(bot: &mut BotProcess, label: &str) -> Result<()> {
    let identity = bot
        .client
        .identify(IdentifyRequest {
            protocol_version: PROTOCOL_VERSION,
        })
        .await?
        .into_inner();

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
    status: MatchStatus,
) {
    let request = MatchEndedRequest {
        match_id: match_id.to_string(),
        outcome: outcome_for(status, chapter),
        reason: reason_for(status),
    };

    if let Err(error) = bot.client.match_ended(request).await {
        eprintln!("could not notify bot {chapter:?} that the match ended: {error}");
    }
}

fn reason_for(status: MatchStatus) -> i32 {
    let reason = match status {
        MatchStatus::Winner { reason, .. } | MatchStatus::Draw { reason } => reason,
        MatchStatus::InProgress => {
            return MatchEndReason::Unspecified as i32;
        }
    };

    match reason {
        EndReason::Elimination => MatchEndReason::Elimination as i32,
        EndReason::TurnCap => MatchEndReason::TurnCap as i32,
    }
}

fn outcome_for(status: MatchStatus, recipient: Chapter) -> i32 {
    match status {
        MatchStatus::Winner { chapter, .. } if chapter == recipient => MatchOutcome::Win as i32,
        MatchStatus::Winner { .. } => MatchOutcome::Loss as i32,
        MatchStatus::Draw { .. } => MatchOutcome::Draw as i32,
        MatchStatus::InProgress => MatchOutcome::Unspecified as i32,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn result_is_mapped_from_each_bots_perspective() {
        let status = MatchStatus::Winner {
            chapter: Chapter::A,
            reason: EndReason::Elimination,
        };

        assert_eq!(outcome_for(status, Chapter::A), MatchOutcome::Win as i32);
        assert_eq!(outcome_for(status, Chapter::B), MatchOutcome::Loss as i32);
        assert_eq!(reason_for(status), MatchEndReason::Elimination as i32);
    }
}
