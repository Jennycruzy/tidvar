use crate::{App, Failure};
use axum::{
    Json,
    extract::{Path, State},
};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tidvar_core::{params::CHAT_PRICE, transport::Authenticated};
use tidvar_payee::PayeeError;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Longest prompt accepted, in characters.
pub const MAX_PROMPT_CHARS: usize = 2_000;
const MAX_ANSWER_TOKENS: u32 = 400;
const MODEL_TIMEOUT: Duration = Duration::from_secs(150);
const SYSTEM: &str = "You are Tidvar Chat, a private assistant running on the service's own server; the user pays one cent per answer in shielded Zcash. Facts you may use: Zcash is a cryptocurrency with shielded (private) payments. A payment channel locks money once on chain, then sends many signed off-chain payments, and settles with one closing transaction; Tidvar is a payment channel on Zcash. Answer accurately, plainly and briefly. If you are not sure, say so.";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatRequest {
    request: Authenticated,
    prompt: Option<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Body {
    step: String,
    prompt_sha256: String,
    price: u64,
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
        _ => Failure::bad(),
    }
}
fn respond(reply: &[u8], answer: Option<String>) -> Result<Json<serde_json::Value>, Failure> {
    let reply: serde_json::Value =
        serde_json::from_slice(reply).map_err(|_| Failure::internal())?;
    Ok(Json(serde_json::json!({"reply": reply, "answer": answer})))
}
fn refuse(
    app: &App,
    request: &Authenticated,
    error: &str,
) -> Result<Json<serde_json::Value>, Failure> {
    let mut store = app.store.lock().map_err(|_| Failure::internal())?;
    let reply = store
        .save_reply(
            request,
            serde_json::json!({"step":"chat_refused","error":error}),
        )
        .map_err(failure)?;
    respond(&reply, None)
}
fn model_address() -> String {
    std::env::var("TIDVAR_CHAT_MODEL").unwrap_or_else(|_| "127.0.0.1:27463".to_owned())
}
/// One answer from the local model. The prompt is held only for this call.
async fn ask(prompt: &str) -> Result<String, ()> {
    let body = serde_json::to_vec(&serde_json::json!({
        "messages": [{"role":"system","content":SYSTEM},{"role":"user","content":prompt}],
        "max_tokens": MAX_ANSWER_TOKENS,
        "temperature": 0.7,
        "cache_prompt": false,
    }))
    .map_err(|_| ())?;
    let exchange = async {
        let mut stream = tokio::net::TcpStream::connect(model_address())
            .await
            .map_err(|_| ())?;
        let head = format!(
            "POST /v1/chat/completions HTTP/1.0\r\nHost: 127.0.0.1\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n",
            body.len()
        );
        stream.write_all(head.as_bytes()).await.map_err(|_| ())?;
        stream.write_all(&body).await.map_err(|_| ())?;
        let mut raw = Vec::new();
        stream
            .take(1 << 20)
            .read_to_end(&mut raw)
            .await
            .map_err(|_| ())?;
        Ok::<_, ()>(raw)
    };
    let raw = tokio::time::timeout(MODEL_TIMEOUT, exchange)
        .await
        .map_err(|_| ())??;
    let split = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or(())?;
    let status = std::str::from_utf8(&raw[..split]).map_err(|_| ())?;
    if !status.starts_with("HTTP/1.1 200") && !status.starts_with("HTTP/1.0 200") {
        return Err(());
    }
    let value: serde_json::Value = serde_json::from_slice(&raw[split + 4..]).map_err(|_| ())?;
    let answer = value["choices"][0]["message"]["content"]
        .as_str()
        .ok_or(())?
        .trim()
        .to_owned();
    if answer.is_empty() {
        Err(())
    } else {
        Ok(answer)
    }
}

pub async fn handle(
    State(app): State<App>,
    Path(id): Path<String>,
    Json(chat): Json<ChatRequest>,
) -> Result<Json<serde_json::Value>, Failure> {
    let request = chat.request;
    if hex::decode(id).map_err(|_| Failure::bad())?.as_slice() != request.message.tab_id.0 {
        return Err(Failure::bad());
    }
    let tab = request.message.tab_id.0;
    {
        let store = app.store.lock().map_err(|_| Failure::internal())?;
        if let Some(reply) = store.cached_reply(&request).map_err(failure)? {
            return respond(&reply, None);
        }
    }
    let body: Body =
        serde_json::from_value(request.message.body.clone()).map_err(|_| Failure::bad())?;
    if body.step != "chat" || body.price != CHAT_PRICE {
        return Err(Failure::bad());
    }
    let Some(prompt) = chat.prompt else {
        return refuse(
            &app,
            &request,
            "The message was not received. It was not charged; send it again.",
        );
    };
    let prompt = prompt.trim().to_owned();
    if prompt.is_empty() || prompt.chars().count() > MAX_PROMPT_CHARS {
        return refuse(
            &app,
            &request,
            "Write a message of at most 2,000 characters. It was not charged.",
        );
    }
    if hex::encode(Sha256::digest(prompt.as_bytes())) != body.prompt_sha256 {
        return refuse(
            &app,
            &request,
            "The message does not match its signed request. It was not charged.",
        );
    }
    {
        let mut store = app.store.lock().map_err(|_| Failure::internal())?;
        let balance = store.balance(&tab, now()?).map_err(failure)?;
        if balance.closing || balance.paid.value().saturating_sub(balance.used.value()) < CHAT_PRICE
        {
            drop(store);
            return refuse(
                &app,
                &request,
                "Pay for this message first: the tab needs 1,000 zatoshi of prepaid credit.",
            );
        }
    }
    let answer = {
        let _turn = app.chat.acquire().await.map_err(|_| Failure::internal())?;
        ask(&prompt).await
    };
    drop(prompt);
    let Ok(answer) = answer else {
        return refuse(
            &app,
            &request,
            "The model could not answer. The message was not charged; try again.",
        );
    };
    let charged = {
        let mut store = app.store.lock().map_err(|_| Failure::internal())?;
        store.charge_and_reply(&request, CHAT_PRICE, now()?, |balance| {
            serde_json::json!({"step":"chat_answered","price":CHAT_PRICE,"balance":balance})
        })
    };
    match charged {
        Ok(reply) => respond(&reply, Some(answer)),
        Err(PayeeError::Amount(_)) => refuse(
            &app,
            &request,
            "The tab's prepaid credit ran out before the answer was ready. It was not charged.",
        ),
        Err(error) => Err(failure(error)),
    }
}
