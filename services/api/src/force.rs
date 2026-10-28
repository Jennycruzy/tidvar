use crate::{App, Failure, close::publish, payments};
use rand_core::OsRng;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tidvar_chain::sync::SyncState;
use tidvar_core::{
    iou::{Kind, TransactionContext},
    params,
};
use tidvar_payee::PayeeError;

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
        _ => Failure::bad(),
    }
}
/// Finishes the newest IOU the payer signed into a complete transaction.
async fn finish_newest(
    app: &App,
    client: &mut tidvar_chain::native::Client,
    tab_id: [u8; 32],
    freeze: bool,
) -> Result<(zcash_primitives::transaction::Transaction, u64, u64), Failure> {
    let (tab, request) = {
        let store = app.store.lock().map_err(|_| Failure::internal())?;
        let tab = store
            .load(&tab_id)
            .map_err(failure)?
            .ok_or_else(Failure::bad)?;
        let request = store
            .newest_iou(&tab_id)
            .map_err(failure)?
            .ok_or_else(Failure::bad)?;
        (tab, request)
    };
    let state = payments::scan(app, client, &tab).await?;
    let tree = client
        .tree(request.anchor_height)
        .await
        .map_err(|_| Failure::unavailable())?;
    let anchor = SyncState::new(&tab.keys.public.viewing_key, tree)
        .map_err(|_| Failure::unavailable())?
        .root();
    let target = u32::try_from(state.height)
        .ok()
        .and_then(|height| height.checked_add(1))
        .ok_or_else(Failure::bad)?;
    let notes = state.funding_notes().map_err(|_| Failure::unavailable())?;
    let fee = params::close_fee(u8::try_from(notes.len()).map_err(|_| Failure::bad())?)
        .map_err(|_| Failure::bad())?
        .value();
    let worker = app.clone();
    tokio::task::spawn_blocking(move || {
        let app = worker;
        let (checked, request) = app
            .store
            .lock()
            .map_err(|_| Failure::internal())?
            .sign_forced(
                &tab_id,
                &notes,
                TransactionContext {
                    target,
                    kind: Kind::Payment,
                    anchor,
                },
                now()?,
                freeze,
                &mut OsRng,
            )
            .map_err(failure)?;
        let proven = app.proofs.prove(checked, &mut OsRng).map_err(failure)?;
        let transaction = app.proofs.extract(proven, &mut OsRng).map_err(failure)?;
        Ok((transaction, request.amount.value(), fee))
    })
    .await
    .map_err(|_| Failure::internal())?
}
/// Closes a tab with the newest IOU the payer signed, without the payer.
pub async fn force_close(app: &App, tab_id: [u8; 32]) -> Result<serde_json::Value, Failure> {
    let _permit = app.work.acquire().await.map_err(|_| Failure::internal())?;
    let mut client = app.chain.lock().await.clone();
    let stored = app
        .store
        .lock()
        .map_err(|_| Failure::internal())?
        .closing_transaction(&tab_id)
        .map_err(failure)?;
    if let Some(closing) = stored {
        return publish(app, &mut client, &tab_id, closing).await;
    }
    let (transaction, amount, fee) = finish_newest(app, &mut client, tab_id, true).await?;
    let closing = app
        .store
        .lock()
        .map_err(|_| Failure::internal())?
        .save_close(&tab_id, &transaction, amount, fee)
        .map_err(failure)?;
    publish(app, &mut client, &tab_id, closing).await
}
/// Hands a finished transaction (never a key) to the watchtower's queue folder.
pub async fn pack(app: &App, tab_id: [u8; 32]) -> Result<String, Failure> {
    let _permit = app.work.acquire().await.map_err(|_| Failure::internal())?;
    let mut client = app.chain.lock().await.clone();
    let (transaction, amount, _) = finish_newest(app, &mut client, tab_id, false).await?;
    let (txid, raw, watch_ms, expiry) = app
        .store
        .lock()
        .map_err(|_| Failure::internal())?
        .save_pack(&tab_id, &transaction, amount)
        .map_err(failure)?;
    let queue = std::path::PathBuf::from(
        std::env::var("TIDVAR_WATCH_QUEUE")
            .unwrap_or_else(|_| "/root/tidvar-watch/queue".to_owned()),
    );
    std::fs::create_dir_all(&queue).map_err(|_| Failure::internal())?;
    let entry = serde_json::json!({
        "network": app.offer.network,
        "txid": txid,
        "raw": hex::encode(raw),
        "broadcast_after_ms": watch_ms,
        "expiry": expiry,
    });
    let name = format!("{}.json", &txid[..16]);
    let temporary = queue.join(format!(".{name}"));
    std::fs::write(
        &temporary,
        serde_json::to_vec_pretty(&entry).map_err(|_| Failure::internal())?,
    )
    .map_err(|_| Failure::internal())?;
    std::fs::rename(&temporary, queue.join(&name)).map_err(|_| Failure::internal())?;
    Ok(txid)
}
/// Once a minute, closes every tab that reached its closing deadline, so the service is paid
/// before the payer's refund time even if the payer never returns.
pub async fn watch_deadlines(app: App) {
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
        // Keeps the network upgrade in force current for IOU checks (mainnet NU7).
        let mut client = app.chain.lock().await.clone();
        let _ = client.info().await;
        let packing = now().and_then(|now| {
            app.store
                .lock()
                .map_err(|_| Failure::internal())?
                .packing_due(now)
                .map_err(failure)
        });
        for tab in packing.unwrap_or_default() {
            match pack(&app, tab).await {
                Ok(_) => eprintln!("Watchtower packed a finished close."),
                Err(error) => eprintln!("Watchtower pack not finished: {}", error.1),
            }
        }
        let due = match now().and_then(|now| {
            app.store
                .lock()
                .map_err(|_| Failure::internal())?
                .forced_close_due(now)
                .map_err(failure)
        }) {
            Ok(due) => due,
            Err(_) => continue,
        };
        for tab in due {
            match force_close(&app, tab).await {
                Ok(reply) => {
                    eprintln!("Forced close: {}", reply["step"].as_str().unwrap_or("done"))
                }
                Err(error) => eprintln!("Forced close not finished: {}", error.1),
            }
        }
    }
}
