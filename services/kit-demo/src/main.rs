//! The tidvar-402 example: a route that costs 2,000 zat per request, paid from a Tidvar tab.
#![forbid(unsafe_code)]

use axum::{Router, middleware::from_fn_with_state, routing::get};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let data = std::env::var("TIDVAR_DATA_DIR").unwrap_or_else(|_| "/root/tidvar-data".to_owned());
    let offer = std::env::var("TIDVAR_OFFER_JSON")?;
    let paywall = tidvar_402::Paywall::open(
        &std::path::Path::new(&data).join("payee.sqlite"),
        &offer,
        2_000,
    )?;
    let app = Router::new()
        .route("/paid/fact", get(|| async { "A Zcash payment channel moves value off chain and settles in one shielded transaction." }))
        .layer(from_fn_with_state(paywall, tidvar_402::charge));
    let listener = tokio::net::TcpListener::bind(
        std::env::var("TIDVAR_KIT_LISTEN").unwrap_or_else(|_| "127.0.0.1:27465".to_owned()),
    )
    .await?;
    axum::serve(listener, app).await?;
    Ok(())
}
