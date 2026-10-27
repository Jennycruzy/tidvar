//! `tidvar`: commands that need no service.
//!
//! `tidvar send-refund <refund.pczt> [--network testnet|mainnet]` proves a refund the browser
//! signed alone (after the time seal opened) and sends it to a public Zcash server.
#![forbid(unsafe_code)]

use anyhow::{Context, Result, bail};
use rand_core::OsRng;
use tidvar_chain::native::{Client, server};
use tidvar_core::record::Network;
use tidvar_payee::proof::Proofs;

fn network(arguments: &[String]) -> Result<Network> {
    match arguments.iter().position(|value| value == "--network") {
        None => Ok(Network::Testnet),
        Some(index) => arguments
            .get(index + 1)
            .and_then(|name| Network::from_name(name))
            .context("--network must be testnet or mainnet"),
    }
}

async fn send_refund(path: &str, network: Network) -> Result<()> {
    let bytes = std::fs::read(path).with_context(|| format!("could not read {path}"))?;
    let pczt = pczt::Pczt::parse(&bytes)
        .map_err(|_| anyhow::anyhow!("this is not a signed refund file"))?;
    eprintln!("Proving the refund (about a minute on a laptop)...");
    let proofs = Proofs::build();
    let proven = proofs
        .prove_pczt(pczt, &mut OsRng)
        .map_err(|_| anyhow::anyhow!("the refund could not be proven; it may already be spent"))?;
    let transaction = proofs
        .extract(proven, &mut OsRng)
        .map_err(|_| anyhow::anyhow!("the refund is not fully signed"))?;
    let mut raw = Vec::new();
    transaction.write(&mut raw)?;
    let mut client = Client::connect_to(server(network), network)
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let txid = client
        .broadcast(raw)
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    println!("{txid}");
    eprintln!("Refund sent. It returns the tab to your own address once mined.");
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let arguments: Vec<String> = std::env::args().skip(1).collect();
    match arguments.first().map(String::as_str) {
        Some("send-refund") => {
            let path = arguments
                .get(1)
                .context("give the refund file the page downloaded")?;
            send_refund(path, network(&arguments)?).await
        }
        _ => bail!("usage: tidvar send-refund <refund.pczt> [--network testnet|mainnet]"),
    }
}
