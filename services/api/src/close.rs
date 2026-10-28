use crate::{App, Failure, bytes_response, payments};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::Response,
};
use rand_core::OsRng;
use serde::Deserialize;
use std::time::{SystemTime, UNIX_EPOCH};
use tidvar_chain::{ChainError, native::Client, sync::SyncState};
use tidvar_core::{
    iou::{self, Kind, TransactionContext},
    params,
    transport::Authenticated,
};
use tidvar_payee::{PayeeError, close::Closing};

#[derive(Deserialize)]
#[serde(tag = "step", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    #[serde(rename = "close_start")]
    Start,
    #[serde(rename = "close_submit")]
    Submit {
        payment: iou::Request,
        /// Renew: the payer's next tab (opened with this service) receives the change.
        #[serde(default)]
        renew: Option<String>,
    },
    #[serde(rename = "close_status")]
    Status,
}
fn now() -> Result<u64, Failure> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|time| u64::try_from(time.as_millis()).ok())
        .ok_or_else(Failure::internal)
}
fn failure(error: PayeeError) -> Failure {
    match error {
        PayeeError::Storage => Failure::internal(),
        PayeeError::Exchange => Failure::conflict(),
        _ => Failure::bad(),
    }
}
pub(crate) async fn publish(
    app: &App,
    client: &mut Client,
    id: &[u8; 32],
    mut closing: Closing,
) -> Result<serde_json::Value, Failure> {
    let tip = client
        .latest()
        .await
        .map_err(|_| Failure::unavailable())?
        .height;
    match client.transaction(&closing.txid).await {
        Ok(actual) => {
            if actual.data != closing.raw {
                return Err(Failure::unavailable());
            }
            if actual.height > 0 && actual.height <= tip {
                app.store
                    .lock()
                    .map_err(|_| Failure::internal())?
                    .mark_close_mined(id, &closing.txid, actual.height)
                    .map_err(failure)?;
                closing.height = actual.height;
            } else {
                closing.height = 0;
            }
        }
        Err(ChainError::TransactionMissing) => {
            closing.height = 0;
            if tip >= u64::from(closing.expiry) {
                return Ok(serde_json::json!({"step":"close_expired","close":closing}));
            }
            let txid = client
                .broadcast(closing.raw.clone())
                .await
                .map_err(|_| Failure::unavailable())?;
            if txid != closing.txid {
                return Err(Failure::internal());
            }
        }
        Err(_) => return Err(Failure::unavailable()),
    }
    Ok(
        serde_json::json!({"step":if closing.height>0 {"close_mined"} else {"close_submitted"},"close":closing}),
    )
}
pub async fn handle(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(request): Json<Authenticated>,
) -> Result<Response, Failure> {
    if hex::decode(&id).map_err(|_| Failure::bad())?.as_slice() != request.message.tab_id.0 {
        return Err(Failure::bad());
    }
    let _permit = app
        .work
        .clone()
        .try_acquire_owned()
        .map_err(|_| Failure::unavailable())?;
    if let Some(reply) = app
        .store
        .lock()
        .map_err(|_| Failure::internal())?
        .cached_reply(&request)
        .map_err(failure)?
    {
        return Ok(bytes_response("application/json", reply));
    }
    let action: Action =
        serde_json::from_value(request.message.body.clone()).map_err(|_| Failure::bad())?;
    let tab_id = request.message.tab_id.0;
    let mut client = app.chain.lock().await.clone();
    let stored = app
        .store
        .lock()
        .map_err(|_| Failure::internal())?
        .closing_transaction(&tab_id)
        .map_err(failure)?;
    let existing = if let Some(closing) = stored {
        Some(publish(&app, &mut client, &tab_id, closing).await?)
    } else {
        None
    };
    let result=async {
    let body = if let Some(reply) = existing
        .as_ref()
        .filter(|reply| reply["step"] != "close_expired")
    {
        reply.clone()
    } else {
        match action {
            Action::Status => {
                existing.unwrap_or_else(|| serde_json::json!({"step":"close_pending"}))
            }
            Action::Start => {
                let (tab, balance) = {
                    let mut store = app.store.lock().map_err(|_| Failure::internal())?;
                    let tab = store
                        .load(&tab_id)
                        .map_err(failure)?
                        .ok_or_else(Failure::bad)?;
                    let balance = store.close_ledger(&tab_id, now()?).map_err(failure)?;
                    (tab, balance)
                };
                let state = payments::scan(&app, &mut client, &tab).await?;
                let notes = state.funding_notes().map_err(|_| Failure::unavailable())?;
                let commitments = app
                    .store
                    .lock()
                    .map_err(|_| Failure::internal())?
                    .issue_nonces(&tab_id, notes.len(), None, &mut OsRng)
                    .map_err(failure)?;
                serde_json::json!({"step":"close_ready","used":balance.used,"balance":balance,"commitments":commitments,"notes":notes.len()})
            }
            Action::Submit { payment, renew } => {
                let tab = app
                    .store
                    .lock()
                    .map_err(|_| Failure::internal())?
                    .load(&tab_id)
                    .map_err(failure)?
                    .ok_or_else(Failure::bad)?;
                let state = payments::scan(&app, &mut client, &tab).await?;
                if payment.anchor_height > state.height || state.height - payment.anchor_height > 24
                {
                    return Err(Failure(
                        StatusCode::CONFLICT,
                        "Refresh this tab before closing it.",
                    ));
                }
                let tree = client
                    .tree(payment.anchor_height)
                    .await
                    .map_err(|_| Failure::unavailable())?;
                let anchor = SyncState::new(&tab.keys.public.viewing_key, tree)
                    .map_err(|_| Failure::unavailable())?
                    .root();
                let target = u32::try_from(payment.anchor_height)
                    .ok()
                    .and_then(|height| height.checked_add(1))
                    .ok_or_else(Failure::bad)?;
                let notes = payments::counted(&state, payment.messages.len())?;
                let value = iou::notes_value(&notes).map_err(|_| Failure::bad())?.value();
                let fee = params::close_fee(u8::try_from(notes.len()).map_err(|_| Failure::bad())?)
                    .map_err(|_| Failure::bad())?
                    .value();
                let to = match renew {
                    None => None,
                    Some(next) => {
                        let next_id: [u8; 32] = hex::decode(&next)
                            .ok()
                            .and_then(|bytes| bytes.try_into().ok())
                            .ok_or_else(Failure::bad)?;
                        let next_tab = app
                            .store
                            .lock()
                            .map_err(|_| Failure::internal())?
                            .load(&next_id)
                            .map_err(failure)?
                            .ok_or_else(Failure::bad)?;
                        let returned = value
                            .checked_sub(payment.amount.value())
                            .and_then(|rest| rest.checked_sub(fee))
                            .ok_or_else(Failure::bad)?;
                        if next_id == tab_id
                            || next_tab.record.body.offer.address != tab.record.body.offer.address
                            || next_tab.record.body.offer.network != tab.record.body.offer.network
                            || next_tab.record.body.terms.funded.value() != returned
                        {
                            return Err(Failure(
                                StatusCode::CONFLICT,
                                "The next tab does not match what this tab returns. Nothing was signed.",
                            ));
                        }
                        Some(next_tab.keys.public.address.clone())
                    }
                };
                let worker = app.clone();
                let closing = tokio::task::spawn_blocking(move || {
                    let app = worker;
                    let checked = app
                        .store
                        .lock()
                        .map_err(|_| Failure::internal())?
                        .sign_close(
                            &tab_id,
                            &payment,
                            &notes,
                            TransactionContext {
                                target,
                                kind: Kind::Cooperative,
                                anchor,
                            },
                            to.as_deref(),
                            &mut OsRng,
                        )
                        .map_err(failure)?;
                    let proven = app.proofs.prove(checked, &mut OsRng).map_err(failure)?;
                    let transaction = app.proofs.extract(proven, &mut OsRng).map_err(failure)?;
                    app.store
                        .lock()
                        .map_err(|_| Failure::internal())?
                        .save_close(&tab_id, &transaction, payment.amount.value(), fee, value)
                        .map_err(failure)
                })
                .await
                .map_err(|_| Failure::internal())??;
                publish(&app, &mut client, &tab_id, closing).await?
            }
        }
    };
    Ok::<_,Failure>(body)
    }.await;
    let body = match result {
        Ok(body) => body,
        Err(error) if matches!(error.0, StatusCode::BAD_REQUEST | StatusCode::CONFLICT) => {
            serde_json::json!({"step":"close_retry","error":error.1})
        }
        Err(error) => return Err(error),
    };
    let reply = app
        .store
        .lock()
        .map_err(|_| Failure::internal())?
        .save_reply(&request, body)
        .map_err(failure)?;
    Ok(bytes_response("application/json", reply))
}
