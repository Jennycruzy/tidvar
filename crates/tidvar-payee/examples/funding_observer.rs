use orchard::{
    keys::{FullViewingKey, Scope},
    note_encryption::IronwoodDomain,
};
use rusqlite::{Connection, OpenFlags, params};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    error::Error,
    io::{Cursor, Write},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tidvar_chain::{
    native::{Client, TESTNET_SERVER},
    service::RawTransaction,
};
use tidvar_core::record::SignedRecord;
use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::BranchId;

type Result<T> = std::result::Result<T, Box<dyn Error>>;
fn now() -> Result<u64> {
    Ok(u64::try_from(
        SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis(),
    )?)
}
fn emit(value: serde_json::Value) -> Result<()> {
    println!("{value}");
    std::io::stdout().flush()?;
    Ok(())
}
fn received(raw: &[u8], record: &SignedRecord) -> Result<(String, u64)> {
    record.verify()?;
    let viewing = FullViewingKey::from_bytes(&record.body.viewing_key.as_slice().try_into()?)
        .ok_or("Invalid viewing key")?;
    let incoming = viewing.to_ivk(Scope::External).prepare();
    let recipient = viewing.address_at(0u32, Scope::External);
    let mut cursor = Cursor::new(raw);
    let transaction = Transaction::read(&mut cursor, BranchId::Nu7)?;
    if cursor.position() != raw.len() as u64 {
        return Err("Trailing transaction bytes".into());
    }
    let mut value = 0u64;
    if let Some(bundle) = transaction.ironwood_bundle() {
        for action in bundle.actions().iter() {
            if let Some((note, address, _)) = zcash_note_encryption::try_note_decryption(
                &IronwoodDomain::for_action(action),
                &incoming,
                action,
            ) && address == recipient
            {
                value = value
                    .checked_add(note.value().inner())
                    .ok_or("Funding overflow")?;
            }
        }
    }
    Ok((transaction.txid().to_string(), value))
}
fn database() -> Result<Connection> {
    let connection = Connection::open_with_flags(
        std::env::var("TIDVAR_OBSERVER_DATABASE")?,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    connection.busy_timeout(Duration::from_secs(2))?;
    Ok(connection)
}
fn match_funding(
    candidates: &BTreeMap<String, SignedRecord>,
    raw: &RawTransaction,
    seen_at: u64,
    seen: &mut BTreeSet<(String, String)>,
    source: &str,
) -> Result<()> {
    for (id, record) in candidates {
        if let Ok((txid, value)) = received(&raw.data, record)
            && value >= record.body.terms.funded.value()
            && seen.insert((id.clone(), txid.clone()))
        {
            emit(
                json!({"event":"funding_seen","tab_id":id,"txid":txid,"value":value,
                    "seen_at_ms":seen_at,"matched_at_ms":now()?,"source":source,
                    "observational_only":true}),
            )?;
        }
    }
    Ok(())
}
#[tokio::main]
async fn main() -> Result<()> {
    let connection = database()?;
    let after: i64 = std::env::var("TIDVAR_OBSERVER_AFTER_ROW")?.parse()?;
    let mut last = after;
    let mut candidates = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut recent: VecDeque<(RawTransaction, u64)> = VecDeque::new();
    let mut client = Client::connect(TESTNET_SERVER).await?;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(24 * 60 * 60);
    emit(json!({"event":"observer_started","at_ms":now()?,"after_row":after,"testnet":true}))?;
    while tokio::time::Instant::now() < deadline {
        let gap_started = now()?;
        let result = tokio::time::timeout(Duration::from_secs(15), client.mempool()).await;
        let mut stream = match result {
            Ok(Ok(stream)) => stream,
            _ => {
                emit(json!({"event":"stream_unavailable","at_ms":now()?}))?;
                tokio::time::sleep(Duration::from_secs(2)).await;
                continue;
            }
        };
        emit(
            json!({"event":"stream_ready","at_ms":now()?,"connect_ms":now()?.saturating_sub(gap_started)}),
        )?;
        let mut poll = tokio::time::interval(Duration::from_millis(250));
        loop {
            tokio::select! {
                item=stream.message()=>{
                    let at=now()?;
                    match item{
                        Ok(Some(raw))=>{
                            match_funding(&candidates,&raw,at,&mut seen,"GetMempoolStream")?;
                            recent.push_back((raw,at));
                            while recent.len()>256{recent.pop_front();}
                        }
                        result=>{
                            emit(json!({"event":"stream_ended","at_ms":at,"error":result.is_err()}))?;
                            break;
                        }
                    }
                }
                _=poll.tick()=>{
                    if tokio::time::Instant::now()>=deadline{return Ok(());}
                    let mut added=false;
                    let mut statement=connection.prepare("SELECT rowid,id,record FROM tabs WHERE rowid>?1 ORDER BY rowid")?;
                    let rows=statement.query_map(params![last],|row|Ok((row.get::<_,i64>(0)?,row.get::<_,Vec<u8>>(1)?,row.get::<_,Vec<u8>>(2)?)))?;
                    for row in rows{
                        let (sequence,id,bytes)=row?;
                        let record:SignedRecord=serde_json::from_slice(&bytes)?;
                        if record.body.offer.network!=tidvar_core::record::Network::Testnet{return Err("Unexpected network".into());}
                        record.verify()?;
                        candidates.insert(hex::encode(&id),record);
                        added=true;
                        last=sequence;
                        emit(json!({"event":"tab_registered","at_ms":now()?,"tab_id":hex::encode(id)}))?;
                    }
                    if added{
                        for (raw,at) in &recent{match_funding(&candidates,raw,*at,&mut seen,"GetMempoolStream")?;}
                    }
                }
            }
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    #[ignore = "Requires an actual testnet service tab and its historical funding"]
    async fn historical_funding_is_recognized() -> Result<()> {
        let connection = database()?;
        let id = hex::decode(std::env::var("TIDVAR_TAB_ID")?)?;
        let bytes: Vec<u8> =
            connection.query_row("SELECT record FROM tabs WHERE id=?1", params![id], |row| {
                row.get(0)
            })?;
        let record: SignedRecord = serde_json::from_slice(&bytes)?;
        let bytes: Vec<u8> = connection.query_row(
            "SELECT snapshot FROM funding WHERE tab_id=?1",
            params![id],
            |row| row.get(0),
        )?;
        let snapshot: tidvar_chain::sync::Snapshot = serde_json::from_slice(&bytes)?;
        let note = snapshot
            .notes
            .first()
            .ok_or("Missing actual funding note")?;
        let txid = hex::encode(note.txid.iter().copied().rev().collect::<Vec<_>>());
        drop(connection);
        let mut client = Client::connect(TESTNET_SERVER).await?;
        let raw = client.transaction(&txid).await?;
        let (matched, value) = received(&raw.data, &record)?;
        assert_eq!(matched, txid);
        assert_eq!(value, record.body.terms.funded.value());
        let mut candidates = BTreeMap::new();
        candidates.insert(hex::encode(&id), record);
        let mut seen = BTreeSet::new();
        match_funding(&candidates, &raw, now()?, &mut seen, "HistoricalValidation")?;
        match_funding(&candidates, &raw, now()?, &mut seen, "HistoricalValidation")?;
        assert_eq!(seen.len(), 1);
        Ok(())
    }
}
