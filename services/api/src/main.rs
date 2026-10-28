#![forbid(unsafe_code)]
mod chat;
mod close;
mod force;
mod opening;
mod payments;
mod vpn;
use axum::{
    Json, Router,
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use rand_core::OsRng;
use reddsa::{SigningKey, VerificationKey, orchard::SpendAuth};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs::{self, OpenOptions},
    io::{Read, Write},
    net::{IpAddr, SocketAddr},
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Component, Path, PathBuf},
    sync::{Arc, Mutex},
    time::Instant,
};
use tidvar_chain::native::{Client, server};
use tidvar_core::{
    encoding::MAX_MESSAGE_BYTES,
    record::{Network, Offer},
};
use tidvar_payee::store::Store;
use tokio::sync::{Mutex as AsyncMutex, Semaphore};
use zeroize::Zeroizing;

#[derive(Clone)]
pub struct App {
    bandwidth: tidvar_payee::usage::Limits,
    offer: Arc<Offer>,
    identity: Arc<SigningKey<SpendAuth>>,
    chain: Arc<AsyncMutex<Client>>,
    store: Arc<Mutex<Store>>,
    exchanges: Arc<Mutex<opening::Exchanges>>,
    work: Arc<Semaphore>,
    chat: Arc<Semaphore>,
    limits: Arc<Mutex<BTreeMap<IpAddr, Limits>>>,
    web: Arc<PathBuf>,
    judges: Arc<PathBuf>,
    evidence: Arc<(Vec<serde_json::Value>, BTreeSet<Vec<u8>>)>,
    proofs: Arc<tidvar_payee::proof::Proofs>,
    payments: Arc<Mutex<PaymentWindows>>,
}
type PaymentWindows = BTreeMap<[u8; 32], (Instant, u32)>;
struct Limits {
    since: Instant,
    ids: BTreeSet<[u8; 32]>,
}
impl App {
    fn payment_limit(&self, id: [u8; 32]) -> Result<(), Failure> {
        let mut payments = self.payments.lock().map_err(|_| Failure::internal())?;
        payments.retain(|_, (since, _)| since.elapsed().as_secs() < 1);
        let (since, count) = payments.entry(id).or_insert((Instant::now(), 0));
        if since.elapsed().as_secs() >= 1 {
            *since = Instant::now();
            *count = 0;
        }
        if *count >= 10 {
            return Err(Failure(
                StatusCode::TOO_MANY_REQUESTS,
                "This tab is sending payments too quickly. Wait a second and retry.",
            ));
        }
        *count += 1;
        Ok(())
    }

    fn limit(&self, ip: IpAddr, id: [u8; 32]) -> Result<(), Failure> {
        let mut limits = self.limits.lock().map_err(|_| Failure::internal())?;
        limits.retain(|_, window| window.since.elapsed().as_secs() < 3600);
        let window = limits.entry(ip).or_insert_with(|| Limits {
            since: Instant::now(),
            ids: BTreeSet::new(),
        });
        if !window.ids.contains(&id) && window.ids.len() >= 20 {
            return Err(Failure(
                StatusCode::TOO_MANY_REQUESTS,
                "Too many new tabs. Keep your current tab and try later.",
            ));
        }
        window.ids.insert(id);
        Ok(())
    }
}
#[derive(Debug)]
pub struct Failure(StatusCode, &'static str);
impl Failure {
    fn bad() -> Self {
        Self(
            StatusCode::BAD_REQUEST,
            "The request could not be verified. No money moved.",
        )
    }
    fn internal() -> Self {
        Self(
            StatusCode::SERVICE_UNAVAILABLE,
            "The service could not save this step. Keep your backup and try again.",
        )
    }
    fn unavailable() -> Self {
        Self(
            StatusCode::SERVICE_UNAVAILABLE,
            "We can't reach the Zcash server right now. Retrying.",
        )
    }
    fn conflict() -> Self {
        Self(
            StatusCode::CONFLICT,
            "This request does not follow the saved exchange. Retry the same request.",
        )
    }
}
impl IntoResponse for Failure {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({"error":self.1}))).into_response()
    }
}
pub fn bytes_response(content_type: &'static str, body: Vec<u8>) -> Response {
    ([("content-type",content_type),("cache-control","no-store"),("x-content-type-options","nosniff"),("referrer-policy","no-referrer"),("content-security-policy","default-src 'self'; script-src 'self' 'wasm-unsafe-eval'; connect-src 'self' https://api.drand.sh https://drand.cloudflare.com; worker-src 'self'; style-src 'self'; img-src 'self' data:; object-src 'none'; base-uri 'none'; frame-ancestors 'none'")],body).into_response()
}
async fn offer(State(app): State<App>) -> Json<Offer> {
    Json((*app.offer).clone())
}
async fn relay(
    State(app): State<App>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Response, Failure> {
    let method = headers
        .get("x-tidvar-method")
        .and_then(|v| v.to_str().ok())
        .ok_or_else(Failure::bad)?;
    if body.len() < 5 || body[0] != 0 {
        return Err(Failure::bad());
    }
    let length = u32::from_be_bytes(body[1..5].try_into().map_err(|_| Failure::bad())?) as usize;
    if length != body.len() - 5 {
        return Err(Failure::bad());
    }
    let mut client = app.chain.lock().await.clone();
    let reply = client
        .relay(method, &body[5..], &app.evidence.1)
        .await
        .map_err(|_| Failure::unavailable())?;
    Ok(bytes_response("application/grpc-web+proto", reply))
}
async fn files(State(app): State<App>, uri: Uri) -> Result<Response, Failure> {
    let path = if matches!(
        uri.path(),
        "/" | "/open" | "/tab" | "/vpn" | "/judge" | "/check"
    ) {
        "index.html"
    } else {
        uri.path().trim_start_matches('/')
    };
    if Path::new(path)
        .components()
        .any(|part| !matches!(part, Component::Normal(_)))
        || path.starts_with('.')
    {
        return Err(Failure::bad());
    }
    let content_type = match Path::new(path).extension().and_then(|v| v.to_str()) {
        Some("html") => "text/html; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("wasm") => "application/wasm",
        Some("svg") => "image/svg+xml",
        _ => return Err(Failure(StatusCode::NOT_FOUND, "This page was not found.")),
    };
    let body = tokio::fs::read(app.web.join(path))
        .await
        .map_err(|_| Failure(StatusCode::NOT_FOUND, "This page was not found."))?;
    Ok(bytes_response(content_type, body))
}
/// The recorded runs for this network (the same files `tidvar verify` checks). Only their
/// transactions may be fetched through the relay, so `/check` can re-verify them.
fn load_evidence(network: Network) -> (Vec<serde_json::Value>, BTreeSet<Vec<u8>>) {
    let folder = PathBuf::from(
        std::env::var("TIDVAR_EVIDENCE_DIR").unwrap_or_else(|_| "/root/tidvar/evidence".to_owned()),
    )
    .join(match network {
        Network::Mainnet => "mainnet",
        Network::Testnet => "testnet",
    })
    .join("runs");
    let mut paths: Vec<PathBuf> = fs::read_dir(&folder)
        .map(|entries| {
            entries
                .filter_map(|entry| entry.ok().map(|entry| entry.path()))
                .collect()
        })
        .unwrap_or_default();
    paths.retain(|path| path.extension().is_some_and(|ext| ext == "json"));
    paths.sort();
    let mut runs = Vec::new();
    let mut hashes = BTreeSet::new();
    for path in paths {
        let Ok(run) = fs::read(&path)
            .map_err(|_| ())
            .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).map_err(|_| ()))
        else {
            continue;
        };
        for transaction in run["transactions"].as_array().into_iter().flatten() {
            if let Some(mut hash) = transaction["txid"]
                .as_str()
                .and_then(|txid| hex::decode(txid).ok())
            {
                hash.reverse();
                hashes.insert(hash);
            }
        }
        runs.push(run);
    }
    (runs, hashes)
}
async fn evidence(State(app): State<App>) -> Json<Vec<serde_json::Value>> {
    Json(app.evidence.0.clone())
}
/// An encrypted judge-tab backup. Only the judge code (held by the judge) can unlock it.
async fn judge_bundle(
    State(app): State<App>,
    axum::extract::Path(bundle): axum::extract::Path<String>,
) -> Result<Response, Failure> {
    if bundle.len() != 16
        || !bundle
            .bytes()
            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
    {
        return Err(Failure::bad());
    }
    let body = tokio::fs::read(app.judges.join(format!("{bundle}.json")))
        .await
        .map_err(|_| {
            Failure(
                StatusCode::NOT_FOUND,
                "This judge code is not known. Check it and retry.",
            )
        })?;
    Ok(bytes_response("application/json", body))
}
fn identity(path: &Path) -> anyhow::Result<SigningKey<SpendAuth>> {
    if fs::symlink_metadata(path).is_ok_and(|meta| meta.file_type().is_symlink()) {
        anyhow::bail!("The service key path must not be a link.");
    }
    let mut secret = Zeroizing::new([0; 32]);
    match OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
    {
        Ok(mut file) => {
            let key = SigningKey::<SpendAuth>::new(OsRng);
            *secret = key.into();
            file.write_all(secret.as_ref())?;
            file.sync_all()?;
        }
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
            let mut file = OpenOptions::new().read(true).open(path)?;
            if file.metadata()?.len() != 32 || file.metadata()?.permissions().mode() & 0o077 != 0 {
                anyhow::bail!("The service key must have mode 600 and exactly 32 bytes.");
            }
            file.read_exact(secret.as_mut())?;
        }
        Err(error) => return Err(error.into()),
    }
    SigningKey::try_from(*secret).map_err(|_| anyhow::anyhow!("The service key could not be read."))
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let network = Network::from_name(
        &std::env::var("TIDVAR_NETWORK").unwrap_or_else(|_| "testnet".to_owned()),
    )
    .ok_or_else(|| anyhow::anyhow!("TIDVAR_NETWORK must be mainnet or testnet."))?;
    let name = match network {
        Network::Mainnet => "mainnet",
        Network::Testnet => "testnet",
    };
    let address = std::env::var("TIDVAR_SERVICE_ADDRESS")?;
    let key_path = PathBuf::from(
        std::env::var("TIDVAR_SERVICE_KEY")
            .unwrap_or_else(|_| format!("/root/tidvar-private/keys/service-{name}.key")),
    );
    let identity = identity(&key_path)?;
    let published = Offer {
        version: 1,
        network,
        service_name: "Tidvar VPN".to_owned(),
        address,
        maximum_lifetime_ms: 30 * 86_400_000,
        vpn_ms_per_zat: 6000,
        service_key: VerificationKey::from(&identity).into(),
    };
    published.validate()?;
    let data = PathBuf::from(
        std::env::var("TIDVAR_DATA_DIR").unwrap_or_else(|_| "/root/tidvar-data".to_owned()),
    );
    fs::create_dir_all(&data)?;
    fs::set_permissions(&data, fs::Permissions::from_mode(0o700))?;
    let store = Store::open(&data.join("payee.sqlite"))?;
    let chain = Client::connect_to(server(network), network).await?;
    let web = PathBuf::from(
        std::env::var("TIDVAR_WEB_ROOT").unwrap_or_else(|_| "/root/tidvar/web/dist".to_owned()),
    );
    let app = App {
        bandwidth: tidvar_payee::usage::Limits::from_environment()?,
        offer: Arc::new(published),
        identity: Arc::new(identity),
        chain: Arc::new(AsyncMutex::new(chain)),
        store: Arc::new(Mutex::new(store)),
        exchanges: Arc::new(Mutex::new(opening::Exchanges::default())),
        work: Arc::new(Semaphore::new(1)),
        chat: Arc::new(Semaphore::new(1)),
        limits: Arc::new(Mutex::new(BTreeMap::new())),
        web: Arc::new(web),
        judges: Arc::new(data.join("judge")),
        evidence: Arc::new(load_evidence(network)),
        proofs: Arc::new(tidvar_payee::proof::Proofs::build()),
        payments: Arc::new(Mutex::new(BTreeMap::new())),
    };
    let mut arguments = std::env::args().skip(1);
    if arguments.next().as_deref() == Some("force-close") {
        let id: [u8; 32] = hex::decode(arguments.next().unwrap_or_default())?
            .try_into()
            .map_err(|_| anyhow::anyhow!("Give the 64-character tab id."))?;
        let reply = force::force_close(&app, id)
            .await
            .map_err(|error| anyhow::anyhow!(error.1))?;
        println!("{reply}");
        return Ok(());
    }
    tokio::spawn(force::watch_deadlines(app.clone()));
    let router = Router::new()
        .route("/v1/offer", get(offer))
        .route("/v1/tabs", post(opening::handle))
        .route("/v1/tabs/{id}/nonces", get(payments::nonces))
        .route("/v1/tabs/{id}/iou", post(payments::iou))
        .route("/v1/tabs/{id}/vpn", post(vpn::handle))
        .route("/v1/tabs/{id}/close", post(close::handle))
        .route("/v1/tabs/{id}/chat", post(chat::handle))
        .route("/v1/tabs/{id}/settle", post(chat::settle))
        .route("/v1/lwd", post(relay))
        .route("/v1/judge/{bundle}", get(judge_bundle))
        .route("/v1/evidence", get(evidence))
        .fallback(files)
        .layer(DefaultBodyLimit::max(MAX_MESSAGE_BYTES))
        .with_state(app);
    let listen: SocketAddr = std::env::var("TIDVAR_LISTEN")
        .unwrap_or_else(|_| "127.0.0.1:27461".to_owned())
        .parse()?;
    if !listen.ip().is_loopback() {
        anyhow::bail!("The API must listen on loopback.");
    }
    let listener = tokio::net::TcpListener::bind(listen).await?;
    eprintln!("Tidvar {name} API ready.");
    axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .await?;
    Ok(())
}
