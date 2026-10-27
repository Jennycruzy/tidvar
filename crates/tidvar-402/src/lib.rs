//! HTTP 402 for Tidvar tabs: put a price on any axum route.
//!
//! ```ignore
//! let paywall = tidvar_402::Paywall::open("/root/tidvar-data/payee.sqlite", offer_json, 2_000)?;
//! let app = Router::new()
//!     .route("/answer", get(|| async { "42" }))
//!     .layer(axum::middleware::from_fn_with_state(paywall, tidvar_402::charge));
//! ```
//! A request without payment gets `402 Payment Required` with the service's offer and the price.
//! The payer then sends a signed `charge` request from its tab in the `Tidvar-Request` header;
//! the route runs only after the tab's prepaid credit is charged, exactly once per request.
#![forbid(unsafe_code)]

use axum::{
    extract::{Request, State},
    http::{HeaderValue, StatusCode},
    middleware::Next,
    response::{IntoResponse, Response},
};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
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
    price: u64,
}
impl Paywall {
    /// Shares the payee store with the service that opened the tabs.
    pub fn open(store: &Path, offer_json: &str, price: u64) -> Result<Self, PayeeError> {
        Ok(Self {
            store: Arc::new(Mutex::new(Store::open(store)?)),
            offer: HeaderValue::from_str(&URL_SAFE_NO_PAD.encode(offer_json))
                .map_err(|_| PayeeError::Terms)?,
            price,
        })
    }
}
fn refuse(paywall: &Paywall, status: StatusCode, message: &'static str) -> Response {
    let mut response = (status, message).into_response();
    let headers = response.headers_mut();
    headers.insert("tidvar-offer", paywall.offer.clone());
    headers.insert("tidvar-price", HeaderValue::from(paywall.price));
    response
}
/// The middleware: charges the route's price from the payer's tab, then runs the route.
pub async fn charge(State(paywall): State<Paywall>, request: Request, next: Next) -> Response {
    let Some(header) = request.headers().get("tidvar-request") else {
        return refuse(
            &paywall,
            StatusCode::PAYMENT_REQUIRED,
            "Pay from a Tidvar tab to use this route.",
        );
    };
    let Ok(signed) = URL_SAFE_NO_PAD
        .decode(header.as_bytes())
        .map_err(|_| ())
        .and_then(|bytes| serde_json::from_slice::<Authenticated>(&bytes).map_err(|_| ()))
    else {
        return refuse(
            &paywall,
            StatusCode::BAD_REQUEST,
            "The payment header could not be read.",
        );
    };
    let body = &signed.message.body;
    if body["step"] != "charge"
        || body["price"].as_u64() != Some(paywall.price)
        || body["route"].as_str() != Some(request.uri().path())
    {
        return refuse(
            &paywall,
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
            store.charge_and_reply(&signed, paywall.price, now, |balance| {
            serde_json::json!({"step":"charged","price":paywall.price,"balance":balance})
        })
        });
    let reply = match charged {
        Ok(reply) => reply,
        Err(PayeeError::Amount(_)) => {
            return refuse(
                &paywall,
                StatusCode::PAYMENT_REQUIRED,
                "The tab needs more prepaid credit for this route.",
            );
        }
        Err(PayeeError::Exchange) => {
            return refuse(
                &paywall,
                StatusCode::CONFLICT,
                "Send the tab's pending request first.",
            );
        }
        Err(_) => {
            return refuse(
                &paywall,
                StatusCode::PAYMENT_REQUIRED,
                "This tab cannot pay here.",
            );
        }
    };
    let mut response = next.run(request).await;
    if let Ok(value) = HeaderValue::from_str(&URL_SAFE_NO_PAD.encode(reply)) {
        response.headers_mut().insert("tidvar-reply", value);
    }
    response
}
