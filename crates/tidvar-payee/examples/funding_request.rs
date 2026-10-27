use serde_json::json;
use std::{error::Error, path::PathBuf};
use tidvar_core::record::Network;
use tidvar_payee::store::Store;

fn main() -> Result<(), Box<dyn Error>> {
    let id: [u8; 32] = hex::decode(std::env::var("TIDVAR_TAB_ID")?)?
        .try_into()
        .map_err(|_| "Invalid tab identifier")?;
    let store = Store::open(&PathBuf::from(std::env::var("TIDVAR_STATUS_DATABASE")?))?;
    let tab = store.load(&id)?.ok_or("The tab was not found")?;
    tab.record.verify()?;
    if tab.record.body.offer.network != Network::Testnet || tab.record.body.tab_id != id {
        return Err("The signed record is not the requested testnet tab".into());
    }
    println!(
        "{}",
        json!({"network":"TESTNET","id":hex::encode(id),"address":tab.keys.public.address,
        "amount":tab.record.body.terms.funded.value(),"return_address":tab.record.body.terms.return_address,
        "open_ms":tab.record.body.terms.open_ms,"expiry":tab.record.body.expiry})
    );
    Ok(())
}
