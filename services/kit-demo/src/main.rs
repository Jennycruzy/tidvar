//! The tidvar-402 example and proxy.
//!
//! Without `TIDVAR_UPSTREAM` it serves one route, `/paid/fact`, at 2,000 zat per request.
//! With `TIDVAR_UPSTREAM=http://127.0.0.1:3000` it puts an existing HTTP API behind tab payments
//! without changing its code: `TIDVAR_ROUTES='/api/search=3000,/api/*=500'` prices paths (the first
//! match wins, a trailing `*` is a prefix), every other path passes through free, and payment headers
//! are removed before the request is forwarded. `TIDVAR_STRIP=/llm` removes a public path prefix
//! before forwarding, so `/llm/v1/chat/completions` reaches the upstream as `/v1/chat/completions`.
#![forbid(unsafe_code)]

use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{Request, State},
    http::StatusCode,
    middleware::from_fn_with_state,
    response::{IntoResponse, Response},
    routing::get,
};

/// The largest request or response body the proxy forwards.
const BODY_LIMIT: usize = 8 * 1024 * 1024;
/// Headers that belong to the payment or the connection, never to the upstream API.
const DROPPED: [&str; 7] = [
    "tidvar-request",
    "payment-signature",
    "host",
    "connection",
    "content-length",
    "transfer-encoding",
    "keep-alive",
];

#[derive(Clone)]
struct Upstream {
    base: String,
    strip: String,
    client: reqwest::Client,
}

fn routes(text: &str) -> anyhow::Result<Vec<(String, u64)>> {
    text.split(',')
        .filter(|part| !part.trim().is_empty())
        .map(|part| {
            let (path, price) = part
                .trim()
                .split_once('=')
                .ok_or_else(|| anyhow::anyhow!("Write each route as /path=price."))?;
            Ok((path.to_owned(), price.parse()?))
        })
        .collect()
}

async fn forward(State(upstream): State<Upstream>, request: Request) -> Response {
    let (parts, body) = request.into_parts();
    let Ok(body) = to_bytes(body, BODY_LIMIT).await else {
        return (
            StatusCode::PAYLOAD_TOO_LARGE,
            "The request body is too large.",
        )
            .into_response();
    };
    let path = parts
        .uri
        .path_and_query()
        .map_or("/", |value| value.as_str());
    let path = path.strip_prefix(upstream.strip.as_str()).unwrap_or(path);
    let target = format!("{}{}", upstream.base.trim_end_matches('/'), path);
    let mut outgoing = upstream.client.request(parts.method, target).body(body);
    for (name, value) in &parts.headers {
        if !DROPPED.contains(&name.as_str()) {
            outgoing = outgoing.header(name, value);
        }
    }
    let Ok(reply) = outgoing.send().await else {
        return (StatusCode::BAD_GATEWAY, "The upstream API did not answer.").into_response();
    };
    let status = reply.status();
    let headers = reply.headers().clone();
    let Ok(bytes) = reply.bytes().await else {
        return (
            StatusCode::BAD_GATEWAY,
            "The upstream API reply was cut off.",
        )
            .into_response();
    };
    let mut response = Response::new(Body::from(bytes));
    *response.status_mut() = status;
    for (name, value) in &headers {
        if !DROPPED.contains(&name.as_str()) {
            response.headers_mut().insert(name.clone(), value.clone());
        }
    }
    response
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let data = std::env::var("TIDVAR_DATA_DIR").unwrap_or_else(|_| "/root/tidvar-data".to_owned());
    let offer = std::env::var("TIDVAR_OFFER_JSON")?;
    let store = std::path::Path::new(&data).join("payee.sqlite");
    let app = match std::env::var("TIDVAR_UPSTREAM") {
        Ok(base) => {
            let priced =
                routes(&std::env::var("TIDVAR_ROUTES").unwrap_or_else(|_| "/*=1000".to_owned()))?;
            let paywall = tidvar_402::Paywall::with_routes(&store, &offer, priced)?;
            let upstream = Upstream {
                base,
                strip: std::env::var("TIDVAR_STRIP").unwrap_or_default(),
                client: reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(60))
                    .redirect(reqwest::redirect::Policy::none())
                    .build()?,
            };
            Router::new()
                .fallback(forward)
                .with_state(upstream)
                .layer(from_fn_with_state(paywall, tidvar_402::charge))
        }
        Err(_) => {
            let paywall = tidvar_402::Paywall::open(&store, &offer, 2_000)?;
            Router::new()
                .route("/paid/fact", get(|| async { "A Zcash payment channel moves value off chain and settles in one shielded transaction." }))
                .layer(from_fn_with_state(paywall, tidvar_402::charge))
        }
    };
    let listener = tokio::net::TcpListener::bind(
        std::env::var("TIDVAR_KIT_LISTEN").unwrap_or_else(|_| "127.0.0.1:27465".to_owned()),
    )
    .await?;
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    #[test]
    fn routes_parse_in_order() -> anyhow::Result<()> {
        assert_eq!(
            super::routes("/api/search=3000, /api/*=500")?,
            vec![
                ("/api/search".to_owned(), 3_000),
                ("/api/*".to_owned(), 500)
            ]
        );
        assert!(super::routes("/api/search").is_err());
        Ok(())
    }
}
