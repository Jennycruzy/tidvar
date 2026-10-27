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
    let state = payments::scan(app, &mut client, &tab).await?;
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
    let closing = tokio::task::spawn_blocking(move || {
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
                &mut OsRng,
            )
            .map_err(failure)?;
        let proven = app.proofs.prove(checked, &mut OsRng).map_err(failure)?;
        let transaction = app.proofs.extract(proven, &mut OsRng).map_err(failure)?;
        app.store
            .lock()
            .map_err(|_| Failure::internal())?
            .save_close(&tab_id, &transaction, request.amount.value(), fee)
            .map_err(failure)
    })
    .await
    .map_err(|_| Failure::internal())??;
    publish(app, &mut client, &tab_id, closing).await
}
/// Once a minute, closes every tab that reached its closing deadline, so the service is paid
/// before the payer's refund time even if the payer never returns.
pub async fn watch_deadlines(app: App) {
    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;
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
