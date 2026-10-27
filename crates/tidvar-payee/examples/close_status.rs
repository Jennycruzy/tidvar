use rusqlite::{Connection, OpenFlags, params};
use serde_json::json;
use std::{error::Error, io::Cursor};
use tidvar_chain::native::{Client, TESTNET_SERVER};
use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::BranchId;

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let id = hex::decode(std::env::var("TIDVAR_TAB_ID")?)?;
    if id.len() != 32 {
        return Err("Invalid tab identifier".into());
    }
    let connection = Connection::open_with_flags(
        std::env::var("TIDVAR_STATUS_DATABASE")?,
        OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    let (txid,expected,amount,fee,returned):(String,Vec<u8>,u64,u64,u64)=connection.query_row(
        "SELECT txid,raw,amount,fee,returned FROM cooperative_closes WHERE tab_id=?1 ORDER BY sequence DESC LIMIT 1",
        params![id],|row|Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?)),
    )?;
    drop(connection);
    let mut client = Client::connect(TESTNET_SERVER).await?;
    let tip = client.latest().await?.height;
    let actual = client.transaction(&txid).await?;
    if actual.data != expected {
        return Err("The chain transaction differs from the saved close".into());
    }
    let mut cursor = Cursor::new(actual.data.as_slice());
    let parsed = Transaction::read(&mut cursor, BranchId::Nu7)?;
    if parsed.txid().to_string() != txid || cursor.position() != actual.data.len() as u64 {
        return Err("The chain transaction identifier could not be verified".into());
    }
    let mined = actual.height > 0 && actual.height <= tip;
    println!(
        "{}",
        json!({"network":"TESTNET","txid":txid,"height":actual.height,"tip":tip,"mined":mined,
        "confirmations":if mined{tip-actual.height+1}else{0},"exact_bytes_match":true,
        "amount":amount,"fee":fee,"returned":returned,"expiry":u32::from(parsed.expiry_height())})
    );
    Ok(())
}
