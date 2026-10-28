#![cfg(not(target_arch = "wasm32"))]
use std::{error::Error, path::PathBuf};
use tidvar_chain::{
    native::{Client, TESTNET_SERVER},
    service::{self, compact_tx_streamer_client::CompactTxStreamerClient},
};
use tonic::transport::{ClientTlsConfig, Endpoint};

type Result<T> = std::result::Result<T, Box<dyn Error>>;

#[tokio::test]
#[ignore = "Requires an explicitly selected real testnet transaction; may broadcast"]
async fn at4_at11_at15_capture_actual_node_response() -> Result<()> {
    let raw_path = PathBuf::from(std::env::var("AT_RAW_PATH")?);
    let output = PathBuf::from(std::env::var("AT_RESPONSE_PATH")?);
    let raw = std::fs::read(raw_path)?;
    let mut chain = Client::connect(TESTNET_SERVER).await?;
    assert_eq!(chain.info().await?.chain_name, "test");
    let channel = Endpoint::from_static(TESTNET_SERVER)
        .connect_timeout(std::time::Duration::from_secs(15))
        .timeout(std::time::Duration::from_secs(30))
        .tls_config(ClientTlsConfig::new().with_webpki_roots())?
        .connect()
        .await?;
    let mut client = CompactTxStreamerClient::new(channel);
    let response = client
        .send_transaction(service::RawTransaction {
            data: raw,
            height: 0,
        })
        .await;
    let result = match response {
        Ok(response) => {
            let r = response.into_inner();
            serde_json::json!({"network":"TESTNET", "error_code":r.error_code,"error_message":r.error_message})
        }
        Err(error) => {
            serde_json::json!({"network":"TESTNET","grpc_code":format!("{:?}",error.code()),"error_message":error.message()})
        }
    };
    std::fs::write(output, serde_json::to_vec_pretty(&result)?)?;
    println!("{result}");
    if let Ok(code) = std::env::var("AT_EXPECT_CODE") {
        assert_eq!(result["error_code"].as_i64(), Some(code.parse()?));
    }
    Ok(())
}

#[tokio::test]
#[ignore = "Requires a real testnet transaction identifier"]
async fn at3_at4_at15_confirm_actual_transaction() -> Result<()> {
    let txid = std::env::var("AT_TXID")?;
    let mut client = Client::connect(TESTNET_SERVER).await?;
    let tx = client.transaction(&txid).await?;
    assert!(tx.height > 0, "The transaction is not mined yet");
    let parsed = zcash_primitives::transaction::Transaction::read(
        tx.data.as_slice(),
        zcash_protocol::consensus::BranchId::Nu7,
    )?;
    assert_eq!(parsed.txid().to_string(), txid);
    let result =
        serde_json::json!({"network":"TESTNET","txid":txid,"height":tx.height,"mined":tx.height>0});
    std::fs::write(
        std::env::var("AT_RESPONSE_PATH")?,
        serde_json::to_vec_pretty(&result)?,
    )?;
    println!("{result}");
    Ok(())
}
