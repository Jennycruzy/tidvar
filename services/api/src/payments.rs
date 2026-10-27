use crate::{App, Failure, bytes_response};
use axum::{
    Json,
    extract::{ConnectInfo, Path, State},
    http::{HeaderMap, StatusCode},
    response::Response,
};
use rand_core::OsRng;
use serde::Deserialize;
use std::{
    net::SocketAddr,
    time::{SystemTime, UNIX_EPOCH},
};
use tidvar_chain::{native::Client, sync::SyncState};
use tidvar_core::{
    iou::{self, Kind, TransactionContext},
    params,
    transport::Authenticated,
};
use tidvar_payee::{PayeeError, opening::Accepted};

#[derive(Deserialize)]
#[serde(tag = "step", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    Nonces,
    Iou { payment: iou::Request },
}
fn now() -> Result<u64, Failure> {
    u64::try_from(
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Failure::internal())?
            .as_millis(),
    )
    .map_err(|_| Failure::internal())
}
fn failure(error: PayeeError) -> Failure {
    match error {
        PayeeError::Exchange => Failure::conflict(),
        PayeeError::Storage => Failure::internal(),
        PayeeError::Proof => Failure(
            StatusCode::BAD_REQUEST,
            "The service refused a payment because its proof could not be verified. Your money is safe.",
        ),
        _ => Failure::bad(),
    }
}
pub(crate) async fn scan(
    app: &App,
    client: &mut Client,
    tab: &Accepted,
) -> Result<SyncState, Failure> {
    let id = tab.record.body.tab_id;
    let snapshot = app
        .store
        .lock()
        .map_err(|_| Failure::internal())?
        .funding_snapshot(&id)
        .map_err(failure)?;
    let mut state = if let Some(snapshot) = snapshot {
        SyncState::restore(serde_json::from_slice(&snapshot).map_err(|_| Failure::internal())?)
            .map_err(|_| Failure::internal())?
    } else {
        let start = u64::from(tab.record.body.terms.open_height)
            .checked_sub(1)
            .ok_or_else(Failure::bad)?;
        SyncState::new(
            &tab.keys.public.viewing_key,
            client
                .tree(start)
                .await
                .map_err(|_| Failure::unavailable())?,
        )
        .map_err(|_| Failure::unavailable())?
    };
    if state.viewing_key().to_bytes().as_slice() != tab.keys.public.viewing_key {
        return Err(Failure::internal());
    }
    let tip = client
        .latest()
        .await
        .map_err(|_| Failure::unavailable())?
        .height;
    if state.height > tip {
        return Err(Failure::unavailable());
    }
    while state.height < tip {
        let start = state.height + 1;
        for block in client
            .blocks(start, (start + 127).min(tip))
            .await
            .map_err(|_| Failure::unavailable())?
        {
            state.apply(block).map_err(|_| Failure::unavailable())?;
        }
    }
    state
        .verify_tip(
            &client
                .tree(state.height)
                .await
                .map_err(|_| Failure::unavailable())?,
        )
        .map_err(|_| Failure::unavailable())?;
    let snapshot = serde_json::to_vec(&state.snapshot().map_err(|_| Failure::internal())?)
        .map_err(|_| Failure::internal())?;
    app.store
        .lock()
        .map_err(|_| Failure::internal())?
        .save_funding_snapshot(&id, &snapshot)
        .map_err(failure)?;
    let value = state.value().map_err(|_| Failure::unavailable())?;
    if value == 0 {
        if state.notes.iter().any(|note| note.spent) {
            return Err(Failure(
                StatusCode::GONE,
                "This tab has already been spent. Keep its closing transaction and your backup.",
            ));
        }
        return Err(Failure(
            StatusCode::CONFLICT,
            "We haven't seen your payment yet. It shows up after one block (about a minute).",
        ));
    }
    if value != tab.record.body.terms.funded.value() || state.additional_notes > 0 {
        return Err(Failure(
            StatusCode::CONFLICT,
            "The confirmed amount does not match the amount approved for this tab. Keep your backup.",
        ));
    }
    Ok(state)
}
async fn execute(app: App, request: Authenticated, iou_route: bool) -> Result<Response, Failure> {
    let cached = app
        .store
        .lock()
        .map_err(|_| Failure::internal())?
        .cached_reply(&request)
        .map_err(failure)?;
    if let Some(reply) = cached {
        return Ok(bytes_response("application/json", reply));
    }
    let action: Action =
        serde_json::from_value(request.message.body.clone()).map_err(|_| Failure::bad())?;
    if matches!(action, Action::Iou { .. }) != iou_route {
        return Err(Failure::bad());
    }
    if iou_route {
        app.payment_limit(request.message.tab_id.0)?;
    }
    let permit = app.work.clone().try_acquire_owned().map_err(|_| {
        Failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "The service is checking another request. Keep this page open and retry.",
        )
    })?;
    let tab = app
        .store
        .lock()
        .map_err(|_| Failure::internal())?
        .load(&request.message.tab_id.0)
        .map_err(failure)?
        .ok_or_else(Failure::bad)?;
    let mut client = app.chain.lock().await.clone();
    let state = scan(&app, &mut client, &tab).await?;
    let body = match action {
        Action::Nonces => {
            let app = app.clone();
            let id = tab.record.body.tab_id;
            tokio::task::spawn_blocking(move || {
                let mut store=app.store.lock().map_err(|_|Failure::internal())?;
                let balance=store.balance(&id,now()?).map_err(failure)?;
                if balance.closing {return Err(Failure(StatusCode::GONE,"This tab is closing. Keep its transaction and your backup."));}
                let commitments=store.available_nonces(&id,&mut OsRng).map_err(failure)?;
                let usage=store.usage(&id,now()?,app.bandwidth).map_err(failure)?;
                Ok::<_,Failure>(serde_json::json!({"step":"nonces","commitments":commitments,"balance":balance,"height":state.height,"usage":usage}))
            }).await.map_err(|_|Failure::internal())??
        }
        Action::Iou { payment } => match accept(app.clone(), tab, state, client, payment).await {
            Ok(body) => body,
            Err(error) if matches!(error.0, StatusCode::BAD_REQUEST | StatusCode::CONFLICT) => {
                serde_json::json!({"step":"refused","error":error.1,"code":"IOU_REFUSED"})
            }
            Err(error) => return Err(error),
        },
    };
    let reply = app
        .store
        .lock()
        .map_err(|_| Failure::internal())?
        .save_reply(&request, body)
        .map_err(failure)?;
    drop(permit);
    Ok(bytes_response("application/json", reply))
}
fn path_matches(id: &str, request: &Authenticated) -> Result<(), Failure> {
    if hex::decode(id).map_err(|_| Failure::bad())?.as_slice() != request.message.tab_id.0 {
        return Err(Failure::bad());
    }
    Ok(())
}
pub async fn nonces(
    State(app): State<App>,
    ConnectInfo(_peer): ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
    headers: HeaderMap,
) -> Result<Response, Failure> {
    let header = headers
        .get("tidvar-request")
        .and_then(|value| value.to_str().ok())
        .ok_or_else(Failure::bad)?;
    if header.len() > 16_384 {
        return Err(Failure::bad());
    }
    let request: Authenticated = serde_json::from_str(header).map_err(|_| Failure::bad())?;
    path_matches(&id, &request)?;
    execute(app, request, false).await
}
pub async fn iou(
    State(app): State<App>,
    ConnectInfo(_peer): ConnectInfo<SocketAddr>,
    Path(id): Path<String>,
    Json(request): Json<Authenticated>,
) -> Result<Response, Failure> {
    path_matches(&id, &request)?;
    execute(app, request, true).await
}

async fn accept(
    app: App,
    tab: Accepted,
    state: SyncState,
    mut client: Client,
    payment: iou::Request,
) -> Result<serde_json::Value, Failure> {
    Ok({
        let id = tab.record.body.tab_id;
        let hash = payment.hash(&id).map_err(|_| Failure::bad())?;
        let accepted = app
            .store
            .lock()
            .map_err(|_| Failure::internal())?
            .payment_receipt(&id, &hash)
            .map_err(failure)?;
        if let Some(receipt) = accepted {
            serde_json::json!({"step":"accepted","receipt":receipt})
        } else {
            let balance = app
                .store
                .lock()
                .map_err(|_| Failure::internal())?
                .balance(&id, now()?)
                .map_err(failure)?;
            params::check_payment(tab.record.body.terms.funded, balance.paid, payment.amount)
                .map_err(|_| Failure::bad())?;
            if balance.closing {
                return Err(Failure::bad());
            }
            if payment.anchor_height > state.height || state.height - payment.anchor_height > 24 {
                return Err(Failure(
                    StatusCode::CONFLICT,
                    "Refresh the tab's Zcash blocks before making this payment.",
                ));
            }
            let tree = client
                .tree(payment.anchor_height)
                .await
                .map_err(|_| Failure::unavailable())?;
            if tree.height != payment.anchor_height {
                return Err(Failure::unavailable());
            }
            let anchor = SyncState::new(&tab.keys.public.viewing_key, tree)
                .map_err(|_| Failure::unavailable())?
                .root();
            let target = u32::try_from(state.height)
                .ok()
                .and_then(|height| height.checked_add(1))
                .ok_or_else(Failure::bad)?;
            let witness = app
                .store
                .lock()
                .map_err(|_| Failure::internal())?
                .payment_witness(&id)
                .map_err(failure)?;
            let proofs = app.proofs.clone();
            let app = app.clone();
            tokio::task::spawn_blocking(move || {
                let verified = tidvar_payee::payments::verify(
                    &tab,
                    payment,
                    &state.funding_notes().map_err(|_| Failure::unavailable())?,
                    TransactionContext {
                        target,
                        kind: Kind::Payment,
                        anchor,
                    },
                    witness,
                    &proofs,
                    &mut OsRng,
                )
                .map_err(failure)?;
                let receipt = app
                    .store
                    .lock()
                    .map_err(|_| Failure::internal())?
                    .accept_iou(verified, now()?)
                    .map_err(failure)?;
                Ok::<_, Failure>(serde_json::json!({"step":"accepted","receipt":receipt}))
            })
            .await
            .map_err(|_| Failure::internal())??
        }
    })
}
