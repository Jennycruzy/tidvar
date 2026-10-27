#![forbid(unsafe_code)]

pub mod ibe;
pub mod seal;
use drand_core::{beacon::ApiBeacon, chain::ChainInfo};
use serde::Serialize;
use thiserror::Error;

pub const CHAIN_HASH: &str = "52db9ba70e0cc0f6eaf7803dd07447a1f5477735fd3f661792ba94600c84e971";
pub const PUBLIC_KEY: &str = "83cf0f2896adee7eb8b5f01fcad3912212c437e0073e911fb90022d3e760183c8c4b450b6a0a6c3ac6a5776a2d1064510d1fec758c921cc22b0e17e63aaf4bcb5ed66304de9cf809bd274ca73bab4af5a6e9c76a4bc09e76eae8991ef5ece45a";
pub const GENESIS: u64 = 1_692_803_367;
pub const PERIOD: u64 = 3;

#[derive(Debug, Error)]
pub enum SealError {
    #[error(
        "The service's sealed key failed our check, so we didn't open the tab. No money moved."
    )]
    Invalid,
    #[error("The time signal failed verification.")]
    Beacon,
    #[error(transparent)]
    Ibe(#[from] ibe::IbeError),
    #[error(transparent)]
    Encoding(#[from] tidvar_core::encoding::EncodingError),
}

#[derive(Clone, Serialize)]
pub struct VerifiedBeacon {
    round: u64,
    #[serde(with = "tidvar_core::encoding::bytes")]
    signature: Vec<u8>,
}
impl VerifiedBeacon {
    pub fn round(&self) -> u64 {
        self.round
    }
    pub fn signature(&self) -> &[u8] {
        &self.signature
    }
}

pub fn quicknet_public_key() -> Result<Vec<u8>, SealError> {
    hex::decode(PUBLIC_KEY).map_err(|_| SealError::Beacon)
}

pub fn verify_beacon(info: &ChainInfo, beacon: &ApiBeacon) -> Result<VerifiedBeacon, SealError> {
    if beacon.round() == 0
        || hex::encode(info.hash()) != CHAIN_HASH
        || hex::encode(info.public_key()) != PUBLIC_KEY
        || info.genesis_time() != GENESIS
        || info.period() != PERIOD
        || info.scheme_id() != "bls-unchained-g1-rfc9380"
        || !beacon.verify(info.clone()).map_err(|_| SealError::Beacon)?
    {
        return Err(SealError::Beacon);
    }
    Ok(VerifiedBeacon {
        round: beacon.round(),
        signature: beacon.signature(),
    })
}

/// Verifies a beacon fetched from any drand relay, both given as the relay's JSON.
pub fn verify_beacon_json(info: &str, beacon: &str) -> Result<VerifiedBeacon, SealError> {
    let info: ChainInfo = serde_json::from_str(info).map_err(|_| SealError::Beacon)?;
    let beacon: ApiBeacon = serde_json::from_str(beacon).map_err(|_| SealError::Beacon)?;
    verify_beacon(&info, &beacon)
}
pub fn round_at(unix_seconds: u64) -> Result<u64, SealError> {
    unix_seconds
        .checked_sub(GENESIS)
        .map(|delta| delta / PERIOD + 1)
        .ok_or(SealError::Beacon)
}
