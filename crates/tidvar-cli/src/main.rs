//! `tidvar`: commands that need no service.
//!
//! `tidvar send-refund <refund.pczt> [--network testnet|mainnet]` proves a refund the browser
//! signed alone (after the time seal opened) and sends it to a public Zcash server.
//!
//! `tidvar verify [--network testnet|mainnet] [--evidence <dir>]` re-checks every recorded
//! transaction against a public Zcash server and every evidence file against its manifest.
#![forbid(unsafe_code)]

use anyhow::{Context, Result, bail};
use rand_core::OsRng;
use tidvar_chain::native::{Client, server};
use tidvar_core::record::Network;
use tidvar_payee::proof::Proofs;

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Run {
    name: String,
    ending: String,
    transactions: Vec<Recorded>,
    #[serde(default)]
    payments: u64,
    #[serde(default)]
    fees_zat: u64,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct Recorded {
    role: String,
    txid: String,
    height: u64,
}
fn option<'a>(arguments: &'a [String], name: &str) -> Option<&'a str> {
    arguments
        .iter()
        .position(|value| value == name)
        .and_then(|index| arguments.get(index + 1))
        .map(String::as_str)
}
fn check_manifest(evidence: &std::path::Path) -> Result<usize> {
    use sha2::{Digest, Sha256};
    let manifest = evidence.join("MANIFEST.sha256");
    let text = std::fs::read_to_string(&manifest)
        .with_context(|| format!("missing {}", manifest.display()))?;
    let mut count = 0;
    for line in text.lines().filter(|line| !line.trim().is_empty()) {
        let (digest, file) = line
            .split_once("  ")
            .context("manifest lines are \"<sha256>  <path>\"")?;
        if file.contains("..") || file.starts_with('/') {
            bail!("manifest path {file} leaves the evidence folder");
        }
        let bytes =
            std::fs::read(evidence.join(file)).with_context(|| format!("missing {file}"))?;
        if hex::encode(Sha256::digest(&bytes)) != digest {
            bail!("{file} changed since the manifest was written");
        }
        count += 1;
    }
    Ok(count)
}
async fn verify(network: Network, evidence: &std::path::Path) -> Result<()> {
    let files = check_manifest(evidence)?;
    println!("Manifest: {files} evidence files match their SHA-256.");
    let folder = evidence
        .join(match network {
            Network::Mainnet => "mainnet",
            Network::Testnet => "testnet",
        })
        .join("runs");
    let mut paths: Vec<_> = std::fs::read_dir(&folder)
        .with_context(|| format!("missing {}", folder.display()))?
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| path.extension().is_some_and(|ext| ext == "json"))
        .collect();
    paths.sort();
    let mut client = Client::connect_to(server(network), network)
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    let tip = client
        .latest()
        .await
        .map_err(|error| anyhow::anyhow!("{error}"))?
        .height;
    let (mut runs, mut transactions, mut payments, mut fees) = (0u64, 0u64, 0u64, 0u64);
    let mut endings = std::collections::BTreeMap::<String, u64>::new();
    for path in paths {
        let run: Run = serde_json::from_slice(&std::fs::read(&path)?)
            .with_context(|| format!("{} is not a run record", path.display()))?;
        for recorded in &run.transactions {
            let actual = client
                .transaction(&recorded.txid)
                .await
                .map_err(|error| anyhow::anyhow!("{} {}: {error}", run.name, recorded.role))?;
            let parsed = zcash_primitives::transaction::Transaction::read(
                actual.data.as_slice(),
                network
                    .ironwood_branch(u32::try_from(actual.height.max(1)).unwrap_or(u32::MAX))
                    .context("unknown network upgrade")?,
            )?;
            if parsed.txid().to_string() != recorded.txid {
                bail!(
                    "{} {}: the server returned different bytes",
                    run.name,
                    recorded.role
                );
            }
            if actual.height == 0 || actual.height > tip || actual.height != recorded.height {
                bail!(
                    "{} {}: recorded at height {}, found at {}",
                    run.name,
                    recorded.role,
                    recorded.height,
                    actual.height
                );
            }
            transactions += 1;
        }
        println!(
            "ok  {:<34} {:<12} {} transactions",
            run.name,
            run.ending,
            run.transactions.len()
        );
        runs += 1;
        payments += run.payments;
        fees += run.fees_zat;
        *endings.entry(run.ending).or_default() += 1;
    }
    println!(
        "{runs} tabs, {transactions} transactions re-checked on {} at tip {tip}; {payments} off-chain payments; {fees} zat in network fees.",
        server(network)
    );
    for (ending, count) in endings {
        println!("  {ending}: {count}");
    }
    Ok(())
}

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
        Some("verify") => {
            let evidence =
                std::path::PathBuf::from(option(&arguments, "--evidence").unwrap_or("evidence"));
            verify(network(&arguments)?, &evidence).await
        }
        _ => bail!(
            "usage: tidvar verify [--network testnet|mainnet] [--evidence <dir>]\n       tidvar send-refund <refund.pczt> [--network testnet|mainnet]"
        ),
    }
}
