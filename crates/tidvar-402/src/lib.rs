//! HTTP 402 for Tidvar tabs: put a price on any axum route.
//!
//! ```ignore
//! let paywall = tidvar_402::Paywall::open("/root/tidvar-data/payee.sqlite", offer_json, 2_000)?;
//! let app = Router::new()
//!     .route("/answer", get(|| async { "42" }))
//!     .layer(axum::middleware::from_fn_with_state(paywall, tidvar_402::charge));
//! ```
//! A request without payment gets `402 Payment Required` with the service's offer and the price,
//! both as `Tidvar-Offer`/`Tidvar-Price` and as an x402 v2 `PAYMENT-REQUIRED` header (scheme
//! `tidvar-tab`). The payer then sends a signed `charge` request from its tab, in `Tidvar-Request`
//! or inside an x402 `PAYMENT-SIGNATURE` payload; the route runs only after the tab's prepaid credit
//! is charged, exactly once per request.
//!
//! `Paywall::with_routes` prices paths instead of a whole router: the first matching pattern wins,
//! a trailing `*` matches a prefix, and paths with no price pass through free.
#![forbid(unsafe_code)]

use axum::{
    extract::{Request, State},
    http::{HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::{
    Engine,
    engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD},
};
use std::{
    path::Path,
    sync::{Arc, Mutex},
    time::{SystemTime, UNIX_EPOCH},
};
use tidvar_core::transport::Authenticated;
use tidvar_payee::{PayeeError, store::Store};

#[derive(Clone)]
pub struct Paywall {
    store: Arc<Mutex<Store>>,
    offer: HeaderValue,
    network: &'static str,
    pay_to: String,
    routes: Arc<Vec<(String, u64)>>,
}
impl Paywall {
    /// Shares the payee store with the service that opened the tabs; every route costs `price`.
    pub fn open(store: &Path, offer_json: &str, price: u64) -> Result<Self, PayeeError> {
        Self::with_routes(store, offer_json, vec![("*".to_owned(), price)])
    }
    /// Prices paths: `("/api/search", 3000)`, `("/api/*", 500)`; unpriced paths are free.
    pub fn with_routes(
        store: &Path,
        offer_json: &str,
        routes: Vec<(String, u64)>,
    ) -> Result<Self, PayeeError> {
        let offer: serde_json::Value =
            serde_json::from_str(offer_json).map_err(|_| PayeeError::Terms)?;
        if routes.is_empty() || routes.iter().any(|(_, price)| *price == 0) {
            return Err(PayeeError::Terms);
        }
        Ok(Self {
            store: Arc::new(Mutex::new(Store::open(store)?)),
            offer: HeaderValue::from_str(&URL_SAFE_NO_PAD.encode(offer_json))
                .map_err(|_| PayeeError::Terms)?,
            network: if offer["network"] == "Mainnet" {
                "zcash:mainnet"
            } else {
                "zcash:testnet"
            },
            pay_to: offer["address"].as_str().unwrap_or_default().to_owned(),
            routes: Arc::new(routes),
        })
    }
    /// The price of `path`: the first pattern that matches, where a trailing `*` is a prefix.
    pub fn price_for(&self, path: &str) -> Option<u64> {
        route_price(&self.routes, path)
    }
    fn requirement(&self, path: &str, price: u64, error: &str) -> Option<HeaderValue> {
        let required = serde_json::json!({
            "x402Version": 2,
            "error": error,
            "resource": {"url": path, "serviceName": "Tidvar"},
            "accepts": [{
                "scheme": "tidvar-tab",
                "network": self.network,
                "amount": price.to_string(),
                "asset": "ZEC",
                "payTo": self.pay_to,
                "maxTimeoutSeconds": 60,
                "extra": {"unit": "zatoshi", "offer": self.offer.to_str().unwrap_or_default()}
            }]
        });
        HeaderValue::from_str(&STANDARD.encode(required.to_string())).ok()
    }
}
fn route_price(routes: &[(String, u64)], path: &str) -> Option<u64> {
    routes.iter().find_map(|(pattern, price)| {
        let matched = match pattern.strip_suffix('*') {
            Some(prefix) => path.starts_with(prefix),
            None => path == pattern,
        };
        matched.then_some(*price)
    })
}
fn refuse(
    paywall: &Paywall,
    path: &str,
    price: u64,
    status: StatusCode,
    message: &'static str,
) -> Response {
    let mut response = (status, message).into_response();
    let headers = response.headers_mut();
    headers.insert("tidvar-offer", paywall.offer.clone());
    headers.insert("tidvar-price", HeaderValue::from(price));
    if let Some(required) = paywall.requirement(path, price, message) {
        headers.insert("payment-required", required);
    }
    response
}
/// The signed charge, from `Tidvar-Request` or from an x402 v2 `PAYMENT-SIGNATURE` payload
/// whose `payload.request` carries it.
fn signed_charge(request: &Request) -> Option<Result<Authenticated, ()>> {
    if let Some(header) = request.headers().get("tidvar-request") {
        return Some(
            URL_SAFE_NO_PAD
                .decode(header.as_bytes())
                .map_err(|_| ())
                .and_then(|bytes| serde_json::from_slice(&bytes).map_err(|_| ())),
        );
    }
    let header = request.headers().get("payment-signature")?;
    Some(
        STANDARD
            .decode(header.as_bytes())
            .map_err(|_| ())
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|_| ()))
            .and_then(|payload| {
                if payload["x402Version"] != 2 || payload["accepted"]["scheme"] != "tidvar-tab" {
                    return Err(());
                }
                serde_json::from_value(payload["payload"]["request"].clone()).map_err(|_| ())
            }),
    )
}
/// The middleware: charges the route's price from the payer's tab, then runs the route.
pub async fn charge(State(paywall): State<Paywall>, request: Request, next: Next) -> Response {
    let path = request.uri().path().to_owned();
    let Some(price) = paywall.price_for(&path) else {
        return next.run(request).await;
    };
    let x402 = request.headers().contains_key("payment-signature");
    let Some(signed) = signed_charge(&request) else {
        return refuse(
            &paywall,
            &path,
            price,
            StatusCode::PAYMENT_REQUIRED,
            "Pay from a Tidvar tab to use this route.",
        );
    };
    let Ok(signed) = signed else {
        return refuse(
            &paywall,
            &path,
            price,
            StatusCode::BAD_REQUEST,
            "The payment header could not be read.",
        );
    };
    let body = &signed.message.body;
    if body["step"] != "charge"
        || body["price"].as_u64() != Some(price)
        || body["route"].as_str() != Some(path.as_str())
    {
        return refuse(
            &paywall,
            &path,
            price,
            StatusCode::PAYMENT_REQUIRED,
            "Sign a charge for this route and price.",
        );
    }
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| u64::try_from(time.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0);
    let charged = paywall
        .store
        .lock()
        .map_err(|_| PayeeError::Storage)
        .and_then(|mut store| {
            store.charge_and_reply(
                &signed,
                price,
                now,
                |balance| serde_json::json!({"step":"charged","price":price,"balance":balance}),
            )
        });
    let reply = match charged {
        Ok(reply) => reply,
        Err(PayeeError::Amount(_)) => {
            return refuse(
                &paywall,
                &path,
                price,
                StatusCode::PAYMENT_REQUIRED,
                "The tab needs more prepaid credit for this route.",
            );
        }
        Err(PayeeError::Exchange) => {
            return refuse(
                &paywall,
                &path,
                price,
                StatusCode::CONFLICT,
                "Send the tab's pending request first.",
            );
        }
        Err(_) => {
            return refuse(
                &paywall,
                &path,
                price,
                StatusCode::PAYMENT_REQUIRED,
                "This tab cannot pay here.",
            );
        }
    };
    let mut response = next.run(request).await;
    if x402 {
        let settled = serde_json::json!({
            "success": true,
            "transaction": "",
            "network": paywall.network,
            "amount": price.to_string(),
            "payer": URL_SAFE_NO_PAD.encode(signed.message.tab_id.0),
            "extensions": {"tidvar": {"reply": URL_SAFE_NO_PAD.encode(&reply)}}
        });
        if let Ok(value) = HeaderValue::from_str(&STANDARD.encode(settled.to_string())) {
            response.headers_mut().insert("payment-response", value);
        }
    }
    if let Ok(value) = HeaderValue::from_str(&URL_SAFE_NO_PAD.encode(reply)) {
        response.headers_mut().insert("tidvar-reply", value);
    }
    response
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_first_matching_route_sets_the_price() {
        let routes = [
            ("/api/search".to_owned(), 3_000),
            ("/api/*".to_owned(), 500),
        ];
        let price = |path: &str| super::route_price(&routes, path);
        assert_eq!(price("/api/search"), Some(3_000));
        assert_eq!(price("/api/echo"), Some(500));
        assert_eq!(price("/health"), None);
    }
}
