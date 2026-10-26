use crate::{App, Failure};
use axum::{
    Json,
    extract::{ConnectInfo, State},
    http::StatusCode,
};
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    time::{Instant, SystemTime, UNIX_EPOCH},
};
use tidvar_core::{
    keys::{RoundOne, RoundTwo},
    record::{Offer, RecordBody, ServiceSigned, SignedRecord, Terms},
    transport::{Id, Message},
};
use tidvar_payee::opening;
use tidvar_seal::seal::{Challenge, Opening, SealedKey};

#[derive(Serialize, Deserialize)]
#[serde(tag = "step", rename_all = "snake_case", deny_unknown_fields)]
pub enum Request {
    Begin {
        offer: Offer,
        terms: Terms,
        round_one: RoundOne,
    },
    Keys {
        round_two: RoundTwo,
    },
    Challenge {
        challenge: Challenge,
    },
    Sign {
        record: Box<RecordBody>,
    },
    Accept {
        record: Box<SignedRecord>,
    },
}
#[derive(Serialize)]
#[serde(tag = "step", rename_all = "snake_case")]
pub enum Reply {
    Keys {
        round_one: RoundOne,
        round_two: RoundTwo,
    },
    Seal {
        sealed: SealedKey,
        commitment: Id,
    },
    Openings {
        openings: Vec<Opening>,
    },
    Signed {
        partial: Box<ServiceSigned>,
    },
    Accepted {
        tab_id: Id,
    },
}
pub enum Stage {
    Keys(Box<opening::Setup>),
    Sealed(Box<opening::Sealed>),
    Accepted,
}
pub struct Exchange {
    stage: Option<Stage>,
    next_k: u64,
    previous: Option<Id>,
    last_request: Id,
    reply: Vec<u8>,
    created: Instant,
}
#[derive(Default)]
pub struct Exchanges(pub BTreeMap<[u8; 32], Exchange>);
fn now() -> Result<u64, Failure> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Failure::internal())?
            .as_millis(),
    )
    .map_err(|_| Failure::internal())
}
fn encoded(reply: &Message<Reply>) -> Result<Vec<u8>, Failure> {
    serde_json::to_vec(reply).map_err(|_| Failure::internal())
}
pub async fn handle(
    State(app): State<App>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    Json(message): Json<Message<Request>>,
) -> Result<axum::response::Response, Failure> {
    if matches!(message.body, Request::Begin { .. }) {
        app.limit(peer.ip(), message.tab_id.0)?;
    }
    let mut latest = None;
    if let Request::Begin { .. } = &message.body {
        latest = Some(
            app.chain
                .lock()
                .await
                .latest()
                .await
                .map_err(|_| Failure::unavailable())?
                .height,
        );
    }
    let permit = app.work.clone().try_acquire_owned().map_err(|_| {
        Failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "Setup is busy. Keep this page open and try again.",
        )
    })?;
    let app_copy = app.clone();
    let reply = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let app = app_copy;
        let hash = message.hash().map_err(|_| Failure::bad())?;
        let mut exchanges = app.exchanges.lock().map_err(|_| Failure::internal())?;
        exchanges
            .0
            .retain(|_, state| state.created.elapsed().as_secs() < 1800);
        let id = message.tab_id;
        if let Some(exchange) = exchanges.0.get(&id.0) {
            if exchange.last_request == hash {
                return Ok(exchange.reply.clone());
            }
            if !message.follows(id, exchange.next_k, exchange.previous) {
                return Err(Failure::conflict());
            }
        } else if !matches!(
            &message.body,
            Request::Begin { .. } | Request::Accept { .. }
        ) {
            return Err(Failure::conflict());
        }
        let body = match message.body {
            Request::Begin {
                offer,
                terms,
                round_one,
            } => {
                if exchanges.0.contains_key(&id.0)
                    || exchanges.0.len() >= 16
                    || message.k != 1
                    || message.previous.is_some()
                {
                    return Err(Failure::conflict());
                }
                let height = latest.ok_or_else(Failure::internal)?;
                if offer.canonical().map_err(|_| Failure::bad())?
                    != app.offer.canonical().map_err(|_| Failure::internal())?
                    || now()?.abs_diff(terms.open_ms) > 120_000
                    || u64::from(terms.open_height) > height
                    || height - u64::from(terms.open_height) > 1
                {
                    return Err(Failure::bad());
                }
                let (setup, round_one, round_two) =
                    opening::Setup::begin(offer, terms, round_one, &mut OsRng)
                        .map_err(|_| Failure::bad())?;
                exchanges.0.insert(
                    id.0,
                    Exchange {
                        stage: Some(Stage::Keys(Box::new(setup))),
                        next_k: 1,
                        previous: None,
                        last_request: hash,
                        reply: Vec::new(),
                        created: Instant::now(),
                    },
                );
                Reply::Keys {
                    round_one,
                    round_two,
                }
            }
            Request::Keys { round_two } => {
                let exchange = exchanges.0.get_mut(&id.0).ok_or_else(Failure::conflict)?;
                if !matches!(exchange.stage, Some(Stage::Keys(_))) {
                    return Err(Failure::conflict());
                }
                let Some(Stage::Keys(keys)) = exchange.stage.take() else {
                    return Err(Failure::conflict());
                };
                let sealed = keys
                    .finish(round_two, &mut OsRng)
                    .map_err(|_| Failure::bad())?;
                let body = Reply::Seal {
                    sealed: sealed.sealed().clone(),
                    commitment: Id(sealed
                        .sealed()
                        .commitment()
                        .map_err(|_| Failure::internal())?),
                };
                exchange.stage = Some(Stage::Sealed(Box::new(sealed)));
                body
            }
            Request::Challenge { challenge } => {
                let exchange = exchanges.0.get_mut(&id.0).ok_or_else(Failure::conflict)?;
                let Some(Stage::Sealed(sealed)) = &mut exchange.stage else {
                    return Err(Failure::conflict());
                };
                Reply::Openings {
                    openings: sealed.answer(challenge).map_err(|_| Failure::bad())?,
                }
            }
            Request::Sign { record } => {
                let exchange = exchanges.0.get_mut(&id.0).ok_or_else(Failure::conflict)?;
                let Some(Stage::Sealed(sealed)) = &mut exchange.stage else {
                    return Err(Failure::conflict());
                };
                Reply::Signed {
                    partial: Box::new(
                        sealed
                            .sign(*record, &app.identity, &mut OsRng)
                            .map_err(|_| Failure::bad())?,
                    ),
                }
            }
            Request::Accept { record } => {
                let actual = Id(record.body.tab_id);
                if let Some(exchange) = exchanges.0.get_mut(&id.0) {
                    if !matches!(exchange.stage, Some(Stage::Sealed(_))) {
                        return Err(Failure::conflict());
                    }
                    let Some(Stage::Sealed(sealed)) = exchange.stage.take() else {
                        return Err(Failure::conflict());
                    };
                    let accepted = sealed.accept(*record).map_err(|_| Failure::bad())?;
                    app.store
                        .lock()
                        .map_err(|_| Failure::internal())?
                        .save(&accepted)
                        .map_err(|_| Failure::internal())?;
                    exchange.stage = Some(Stage::Accepted);
                } else {
                    let saved = app
                        .store
                        .lock()
                        .map_err(|_| Failure::internal())?
                        .load(&actual.0)
                        .map_err(|_| Failure::internal())?
                        .ok_or_else(Failure::conflict)?;
                    record.verify().map_err(|_| Failure::bad())?;
                    if saved
                        .record
                        .body
                        .canonical()
                        .map_err(|_| Failure::internal())?
                        != record.body.canonical().map_err(|_| Failure::bad())?
                    {
                        return Err(Failure::conflict());
                    }
                }
                Reply::Accepted { tab_id: actual }
            }
        };
        let reply = Message {
            tab_id: id,
            k: message.k.checked_add(1).ok_or_else(Failure::bad)?,
            previous: Some(hash),
            body,
        };
        let reply_hash = reply.hash().map_err(|_| Failure::internal())?;
        let bytes = encoded(&reply)?;
        if let Some(exchange) = exchanges.0.get_mut(&id.0) {
            exchange.next_k = reply.k.checked_add(1).ok_or_else(Failure::bad)?;
            exchange.previous = Some(reply_hash);
            exchange.last_request = hash;
            exchange.reply = bytes.clone();
        }
        Ok::<_, Failure>(bytes)
    })
    .await
    .map_err(|_| Failure::internal())??;
    Ok(crate::bytes_response("application/json", reply))
}
