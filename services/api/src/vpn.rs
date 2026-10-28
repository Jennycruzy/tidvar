use crate::{App, Failure, bytes_response};
use axum::{
    Json,
    extract::{Path, State},
    http::StatusCode,
    response::Response,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use std::time::{SystemTime, UNIX_EPOCH};
use tidvar_core::transport::Authenticated;
use tidvar_payee::PayeeError;

#[derive(Deserialize)]
#[serde(tag = "step", rename_all = "snake_case", deny_unknown_fields)]
enum Action {
    #[serde(rename = "vpn_peer")]
    Peer { public_key: String },
    #[serde(rename = "vpn_pause")]
    Pause,
    #[serde(rename = "vpn_resume")]
    Resume,
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
        PayeeError::Capacity => Failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "The VPN is full. Keep your prepaid time and try again later.",
        ),
        PayeeError::Exchange => Failure::conflict(),
        PayeeError::Storage => Failure::internal(),
        _ => Failure::bad(),
    }
}
fn server() -> Result<serde_json::Value, Failure> {
    let configuration = std::path::Path::new(
        &std::env::var("TIDVAR_DATA_DIR").unwrap_or_else(|_| "/root/tidvar-data".to_owned()),
    )
    .join("vpn.json");
    let bytes = std::fs::read(configuration).map_err(|_| {
        Failure(
            StatusCode::SERVICE_UNAVAILABLE,
            "The VPN is starting. Keep this page open and retry.",
        )
    })?;
    serde_json::from_slice(&bytes).map_err(|_| Failure::internal())
}
pub async fn handle(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(request): Json<Authenticated>,
) -> Result<Response, Failure> {
    if hex::decode(id).map_err(|_| Failure::bad())?.as_slice() != request.message.tab_id.0 {
        return Err(Failure::bad());
    }
    let mut store = app.store.lock().map_err(|_| Failure::internal())?;
    if let Some(reply) = store.cached_reply(&request).map_err(failure)? {
        return Ok(bytes_response("application/json", reply));
    }
    let action: Action =
        serde_json::from_value(request.message.body.clone()).map_err(|_| Failure::bad())?;
    let tab = request.message.tab_id.0;
    let body = match action_body(&mut store, &tab, action, app.bandwidth) {
        Ok(body) => body,
        Err(error)
            if error.0 == StatusCode::BAD_REQUEST || error.1.starts_with("The VPN is full") =>
        {
            serde_json::json!({"step":"vpn_refused","error":error.1})
        }
        Err(error) => return Err(error),
    };
    let reply = store.save_reply(&request, body).map_err(failure)?;
    Ok(bytes_response("application/json", reply))
}
fn action_body(
    store: &mut tidvar_payee::store::Store,
    tab: &[u8; 32],
    action: Action,
    limits: tidvar_payee::usage::Limits,
) -> Result<serde_json::Value, Failure> {
    if !matches!(&action, Action::Pause) {
        let usage = store.usage(tab, now()?, limits).map_err(failure)?;
        if let Some(limit) = usage.limit {
            return Ok(
                serde_json::json!({"step":"vpn_refused","code":limit,"error":limit.message(),"usage":usage}),
            );
        }
    }
    Ok(match action {
        Action::Peer { public_key } => {
            let key: [u8; 32] = STANDARD
                .decode(public_key)
                .map_err(|_| Failure::bad())?
                .try_into()
                .map_err(|_| Failure::bad())?;
            let server = server()?;
            let peer = store.register_peer(tab, key).map_err(failure)?;
            let balance = store.request_vpn_resume(tab, now()?).map_err(failure)?;
            serde_json::json!({
                "step":"vpn_peer",
                "address":format!("{}/32",peer.address().map_err(failure)?),
                "dns":format!("10.{}.0.1", tidvar_payee::store::vpn_net()),
                "server_key":server["server_key"],
                "endpoint":server["endpoint"],
                "balance":balance,
            })
        }
        Action::Pause => {
            let balance = store.pause_vpn(tab, now()?).map_err(failure)?;
            serde_json::json!({"step":"vpn_paused","balance":balance})
        }
        Action::Resume => {
            store.load(tab).map_err(failure)?.ok_or_else(Failure::bad)?;
            let balance = store.request_vpn_resume(tab, now()?).map_err(failure)?;
            serde_json::json!({"step":"vpn_resumed","balance":balance})
        }
    })
}
