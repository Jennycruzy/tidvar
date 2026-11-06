use crate::{App, Failure, close::publish, payments};
use rand_core::OsRng;
use std::collections::{HashMap, HashSet};
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tidvar_chain::sync::SyncState;
use tidvar_core::{
    iou::{self, Kind, TransactionContext},
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
) -> Result<(zcash_primitives::transaction::Transaction, u64, u64, u64), Failure> {
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
    // The newest IOU spends the notes the service had counted when it was signed.
    let notes = payments::counted(&state, request.messages.len())?;
    let value = iou::notes_value(&notes)
        .map_err(|_| Failure::bad())?
        .value();
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
        Ok((transaction, request.amount.value(), fee, value))
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
    let (transaction, amount, fee, value) = finish_newest(app, &mut client, tab_id, true).await?;
    let closing = app
        .store
        .lock()
        .map_err(|_| Failure::internal())?
        .save_close(&tab_id, &transaction, amount, fee, value)
        .map_err(failure)?;
    publish(app, &mut client, &tab_id, closing).await
}
/// Hands a finished transaction (never a key) to the watchtower's queue folder.
pub async fn pack(app: &App, tab_id: [u8; 32]) -> Result<String, Failure> {
    let _permit = app.work.acquire().await.map_err(|_| Failure::internal())?;
    let mut client = app.chain.lock().await.clone();
    let (transaction, amount, _, _) = finish_newest(app, &mut client, tab_id, false).await?;
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
/// Spaces out retries for a tab whose step keeps failing: 2, 4, 8 ... minutes, at most an hour.
#[derive(Default)]
struct Retries(HashMap<[u8; 32], (u64, u32)>);
impl Retries {
    fn waiting(&self, tab: &[u8; 32], now_ms: u64) -> bool {
        self.0
            .get(tab)
            .is_some_and(|(next_ms, _)| now_ms < *next_ms)
    }
    fn failed(&mut self, tab: [u8; 32], now_ms: u64) -> u64 {
        let entry = self.0.entry(tab).or_insert((0, 0));
        entry.1 = entry.1.saturating_add(1);
        let minutes = (1u64 << entry.1.min(6)).min(60);
        entry.0 = now_ms.saturating_add(minutes * 60_000);
        minutes
    }
    fn done(&mut self, tab: &[u8; 32]) {
        self.0.remove(tab);
    }
}
/// Once a minute, closes every tab that reached its closing deadline, so the service is paid
/// before the payer's refund time even if the payer never returns.
pub async fn watch_deadlines(app: App) {
    let mut pack_retries = Retries::default();
    let mut close_retries = Retries::default();
    // Tabs whose funding is already spent on chain (a watchtower close was mined): nothing to do.
    let mut settled: HashSet<[u8; 32]> = HashSet::new();
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
        let clock = now().unwrap_or(0);
        for tab in packing.unwrap_or_default() {
            if pack_retries.waiting(&tab, clock) {
                continue;
            }
            match pack(&app, tab).await {
                Ok(_) => {
                    pack_retries.done(&tab);
                    eprintln!("Watchtower packed a finished close.")
                }
                Err(error) => {
                    let minutes = pack_retries.failed(tab, clock);
                    eprintln!(
                        "Watchtower pack not finished: {} Next try in {minutes} min.",
                        error.1
                    )
                }
            }
        }
        // A watchtower close that reached the chain ends the tab: no more payments or VPN.
        let published = now().and_then(|now| {
            app.store
                .lock()
                .map_err(|_| Failure::internal())?
                .published_packs(now)
                .map_err(failure)
        });
        for (tab, txid) in published.unwrap_or_default() {
            let Ok(found) = client.transaction(&txid).await else {
                continue;
            };
            if found.height == 0 {
                continue;
            }
            let closed = now().and_then(|now| {
                let mut store = app.store.lock().map_err(|_| Failure::internal())?;
                if store.balance(&tab, now).map_err(failure)?.closing {
                    return Ok(false);
                }
                store.close_ledger(&tab, now).map_err(failure)?;
                Ok(true)
            });
            if closed.unwrap_or(false) {
                eprintln!("A watchtower close was mined; the tab is closed.");
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
            if settled.contains(&tab) || close_retries.waiting(&tab, clock) {
                continue;
            }
            match force_close(&app, tab).await {
                Ok(reply) => {
                    close_retries.done(&tab);
                    eprintln!("Forced close: {}", reply["step"].as_str().unwrap_or("done"))
                }
                Err(error) if error.0 == axum::http::StatusCode::GONE => {
                    settled.insert(tab);
                    eprintln!(
                        "A tab past its deadline is already settled on chain; no forced close needed."
                    )
                }
                Err(error) => {
                    let minutes = close_retries.failed(tab, clock);
                    eprintln!(
                        "Forced close not finished: {} Next try in {minutes} min.",
                        error.1
                    )
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Retries;

    #[test]
    fn retries_back_off_to_an_hour_and_reset_on_success() {
        let mut retries = Retries::default();
        let tab = [7u8; 32];
        assert!(!retries.waiting(&tab, 0));
        let waits: Vec<u64> = (0..8).map(|_| retries.failed(tab, 0)).collect();
        assert_eq!(waits, [2, 4, 8, 16, 32, 60, 60, 60]);
        assert!(retries.waiting(&tab, 59 * 60_000));
        assert!(!retries.waiting(&tab, 60 * 60_000));
        retries.done(&tab);
        assert!(!retries.waiting(&tab, 0));
        assert_eq!(retries.failed(tab, 0), 2);
    }
}
