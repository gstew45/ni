use ni_proto::{
    ni::v1::{
        bot_service_server::BotService, GetOrdersRequest, GetOrdersResponse, IdentifyRequest,
        IdentifyResponse, MatchEndedRequest, MatchEndedResponse, NewMatchRequest, NewMatchResponse,
        SubmitReplayRequest, SubmitReplayResponse,
    },
    PROTOCOL_VERSION,
};
use tonic::{Request, Response, Status};

#[derive(Default)]
pub struct ReferenceBot;

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
        _request: Request<NewMatchRequest>,
    ) -> Result<Response<NewMatchResponse>, Status> {
        Ok(Response::new(NewMatchResponse {}))
    }

    async fn get_orders(
        &self,
        request: Request<GetOrdersRequest>,
    ) -> Result<Response<GetOrdersResponse>, Status> {
        Ok(Response::new(GetOrdersResponse {
            turn: request.into_inner().turn,
            orders: vec![],
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
