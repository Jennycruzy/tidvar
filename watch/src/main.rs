//! Tidvar watchtower. It holds finished transactions only, never keys, and needs nothing from
//! the service: if a tab is still open at its watch time, it publishes the service's packed
//! close through a public Zcash server. A close that is already mined, expired, or replaced by
//! the service's own close is moved out of the queue.
#![forbid(unsafe_code)]

use anyhow::{Context, Result};
use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tidvar_chain::{
    ChainError,
    native::{Client, server},
};
use tidvar_core::record::Network;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Entry {
    network: Network,
    txid: String,
    raw: String,
    broadcast_after_ms: u64,
    expiry: u32,
}
fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|time| u64::try_from(time.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}
fn settle(path: &Path, queue: &Path, outcome: &str) -> Result<()> {
    let folder = queue.join(outcome);
    std::fs::create_dir_all(&folder)?;
    std::fs::rename(
        path,
        folder.join(path.file_name().context("queue file has no name")?),
    )?;
    Ok(())
}
async fn visit(path: &Path, queue: &Path) -> Result<()> {
    let entry: Entry = serde_json::from_slice(&std::fs::read(path)?)?;
    if now_ms() < entry.broadcast_after_ms {
        return Ok(());
    }
    let mut client = Client::connect_to(server(entry.network), entry.network)
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let tip = client
        .latest()
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?
        .height;
    match client.transaction(&entry.txid).await {
        Ok(found) if found.height > 0 => {
            eprintln!(
                "Watchtower: close {} mined at {}.",
                &entry.txid[..16],
                found.height
            );
            return settle(path, queue, "mined");
        }
        Ok(_) => return Ok(()),
        Err(ChainError::TransactionMissing) => {}
        Err(error) => anyhow::bail!("{error}"),
    }
    if tip >= u64::from(entry.expiry) {
        eprintln!(
            "Watchtower: close {} expired unpublished.",
            &entry.txid[..16]
        );
        return settle(path, queue, "expired");
    }
    match client.broadcast(hex::decode(&entry.raw)?).await {
        Ok(txid) if txid == entry.txid => eprintln!("Watchtower: published close {}.", &txid[..16]),
        Ok(_) => anyhow::bail!("the server reported a different transaction id"),
        // Usually the tab was already closed another way, so these notes are spent.
        Err(error) => {
            eprintln!("Watchtower: close {} refused ({error}).", &entry.txid[..16]);
            settle(path, queue, "refused")?;
        }
    }
    Ok(())
}
#[tokio::main]
async fn main() -> Result<()> {
    let queue = PathBuf::from(
        std::env::var("TIDVAR_WATCH_QUEUE")
            .unwrap_or_else(|_| "/root/tidvar-watch/queue".to_owned()),
    );
    std::fs::create_dir_all(&queue)?;
    eprintln!("Tidvar watchtower ready; it holds no keys.");
    loop {
        for item in std::fs::read_dir(&queue)? {
            let path = item?.path();
            let name = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("");
            if path.is_file()
                && name.ends_with(".json")
                && !name.starts_with('.')
                && let Err(error) = visit(&path, &queue).await
            {
                eprintln!("Watchtower: {name} will be retried ({error}).");
            }
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}
