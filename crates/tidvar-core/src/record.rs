use crate::{
    encoding::{self, encode_bytes},
    keys::PublicKeys,
    params::{self, Amount, Deadlines, Lifetime},
};
use group::{Group, GroupEncoding};
use pasta_curves::pallas::Point;
use rand_core::{CryptoRng, RngCore};
use reddsa::{Signature, SigningKey, VerificationKey, orchard::SpendAuth};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicU32, Ordering};
use thiserror::Error;
use zcash_address::unified::{self, Container, Encoding};
use zcash_protocol::consensus::{self, BlockHeight, BranchId, NetworkType, NetworkUpgrade};

/// The lowest mainnet height a Zcash server has reported running NU7 (0: not seen yet).
/// NU7's mainnet height was not scheduled in the pinned library; this lets every tab
/// switch to NU7 transactions as soon as the chain does, without a code change.
static MAINNET_NU7: AtomicU32 = AtomicU32::new(0);

/// Records that a Zcash server reported this network's tip at `height` on `branch`.
pub fn observe_branch(network: Network, branch: &str, height: u64) {
    if network != Network::Mainnet || branch != "77190ad9" {
        return;
    }
    let Ok(height) = u32::try_from(height) else {
        return;
    };
    let mut seen = MAINNET_NU7.load(Ordering::Relaxed);
    while seen == 0 || height < seen {
        match MAINNET_NU7.compare_exchange(seen, height, Ordering::Relaxed, Ordering::Relaxed) {
            Ok(_) => break,
            Err(actual) => seen = actual,
        }
    }
}
/// Consensus parameters for a tab: the library's, plus mainnet NU7 once observed.
#[derive(Clone, Copy, Debug)]
pub struct Consensus(Network);
impl consensus::Parameters for Consensus {
    fn network_type(&self) -> NetworkType {
        self.0.kind()
    }
    fn activation_height(&self, upgrade: NetworkUpgrade) -> Option<BlockHeight> {
        match (self.0, upgrade) {
            (Network::Mainnet, NetworkUpgrade::Nu7) => match MAINNET_NU7.load(Ordering::Relaxed) {
                0 => None,
                height => Some(BlockHeight::from_u32(height)),
            },
            (Network::Mainnet, upgrade) => consensus::MAIN_NETWORK.activation_height(upgrade),
            (Network::Testnet, upgrade) => consensus::TEST_NETWORK.activation_height(upgrade),
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Network {
    Mainnet,
    Testnet,
}
impl Network {
    pub fn kind(self) -> NetworkType {
        match self {
            Self::Mainnet => NetworkType::Main,
            Self::Testnet => NetworkType::Test,
        }
    }
    pub fn consensus(self) -> Consensus {
        Consensus(self)
    }
    /// The lightwalletd chain name for this network.
    pub fn chain_name(self) -> &'static str {
        match self {
            Self::Mainnet => "main",
            Self::Testnet => "test",
        }
    }
    /// SLIP-44 coin type used in PCZTs.
    pub fn coin_type(self) -> u32 {
        match self {
            Self::Mainnet => 133,
            Self::Testnet => 1,
        }
    }
    /// The consensus branch at `height` when it carries Ironwood (NU6.3 or later).
    pub fn ironwood_branch(self, height: u32) -> Option<BranchId> {
        match BranchId::for_height(&self.consensus(), BlockHeight::from_u32(height)) {
            branch @ (BranchId::Nu6_3 | BranchId::Nu7) => Some(branch),
            _ => None,
        }
    }
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "mainnet" | "main" => Some(Self::Mainnet),
            "testnet" | "test" => Some(Self::Testnet),
            _ => None,
        }
    }
    fn byte(self) -> u8 {
        match self {
            Self::Mainnet => 0,
            Self::Testnet => 1,
        }
    }
}
#[derive(Debug, Error)]
pub enum RecordError {
    #[error("The tab terms could not be verified. No money moved.")]
    Terms,
    #[error("Use a shielded receive address for the selected network.")]
    Address,
    #[error("The tab signature could not be verified. No money moved.")]
    Signature,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Offer {
    pub version: u32,
    pub network: Network,
    pub service_name: String,
    pub address: String,
    pub maximum_lifetime_ms: u64,
    pub vpn_ms_per_zat: u64,
    #[serde(with = "crate::encoding::fixed32")]
    pub service_key: [u8; 32],
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Terms {
    pub funded: Amount,
    pub open_ms: u64,
    pub lifetime: Lifetime,
    pub open_height: u32,
    pub return_address: String,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecordBody {
    #[serde(with = "crate::encoding::fixed32")]
    pub tab_id: [u8; 32],
    pub offer: Offer,
    pub terms: Terms,
    pub fee_reserve: Amount,
    pub maximum_payment: Amount,
    pub refund_ms: u64,
    pub refund_round: u64,
    pub close_ms: u64,
    pub expiry: u32,
    #[serde(with = "crate::encoding::fixed32")]
    pub group: [u8; 32],
    #[serde(with = "crate::encoding::fixed32")]
    pub payer_share: [u8; 32],
    #[serde(with = "crate::encoding::fixed32")]
    pub service_share: [u8; 32],
    #[serde(with = "crate::encoding::bytes")]
    pub viewing_key: Vec<u8>,
    #[serde(with = "crate::encoding::fixed32")]
    pub seal_commitment: [u8; 32],
    pub challenge: Vec<u8>,
    #[serde(with = "crate::encoding::fixed32")]
    pub payer_record_key: [u8; 32],
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceSigned {
    pub body: RecordBody,
    #[serde(with = "crate::encoding::bytes")]
    pub signature: Vec<u8>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedRecord {
    pub body: RecordBody,
    #[serde(with = "crate::encoding::bytes")]
    pub service_signature: Vec<u8>,
    #[serde(with = "crate::encoding::bytes")]
    pub payer_signature: Vec<u8>,
}

pub fn receiver(address: &str, network: Network) -> Result<orchard::Address, RecordError> {
    let (found, _, address) =
        unified::Address::decode(address).map_err(|_| RecordError::Address)?;
    if found != network.kind() {
        return Err(RecordError::Address);
    }
    let raw = address
        .items()
        .iter()
        .find_map(|item| match item {
            unified::Receiver::Orchard(bytes) => Some(*bytes),
            _ => None,
        })
        .ok_or(RecordError::Address)?;
    Option::from(orchard::Address::from_raw_address_bytes(&raw)).ok_or(RecordError::Address)
}
fn encoded(fields: &[&[u8]]) -> Result<Vec<u8>, RecordError> {
    encode_bytes(fields).map_err(|_| RecordError::Terms)
}
fn point(bytes: &[u8; 32]) -> Result<Point, RecordError> {
    Option::<Point>::from(Point::from_bytes(bytes))
        .filter(|p| !bool::from(p.is_identity()))
        .ok_or(RecordError::Terms)
}
impl Offer {
    pub fn validate(&self) -> Result<(), RecordError> {
        if self.version != 1
            || self.service_name.is_empty()
            || self.service_name.len() > 128
            || self.maximum_lifetime_ms < Lifetime::Days3.seconds() * 1000
            || self.maximum_lifetime_ms > Lifetime::Days30.seconds() * 1000
            || self.vpn_ms_per_zat != params::VPN_UNIT_MS
        {
            return Err(RecordError::Terms);
        }
        receiver(&self.address, self.network)?;
        VerificationKey::<SpendAuth>::try_from(self.service_key)
            .map_err(|_| RecordError::Signature)?;
        point(&self.service_key)?;
        Ok(())
    }
    pub fn canonical(&self) -> Result<Vec<u8>, RecordError> {
        self.validate()?;
        let mut out = self.version.to_le_bytes().to_vec();
        out.push(self.network.byte());
        out.extend(encoded(&[
            self.service_name.as_bytes(),
            self.address.as_bytes(),
        ])?);
        out.extend(self.maximum_lifetime_ms.to_le_bytes());
        out.extend(self.vpn_ms_per_zat.to_le_bytes());
        out.extend(self.service_key);
        Ok(out)
    }
}
impl Terms {
    pub fn validate(&self, offer: &Offer) -> Result<Deadlines, RecordError> {
        offer.validate()?;
        params::spendable(self.funded).map_err(|_| RecordError::Terms)?;
        if self.lifetime.seconds() * 1000 > offer.maximum_lifetime_ms
            || (self.lifetime == Lifetime::Minutes15 && offer.network != Network::Testnet)
        {
            return Err(RecordError::Terms);
        }
        receiver(&self.return_address, offer.network)?;
        Deadlines::new(self.open_ms, self.lifetime.seconds() * 1000).map_err(|_| RecordError::Terms)
    }
    pub fn canonical(&self) -> Result<Vec<u8>, RecordError> {
        let mut out = self.funded.value().to_le_bytes().to_vec();
        out.extend(self.open_ms.to_le_bytes());
        out.extend((self.lifetime.seconds() * 1000).to_le_bytes());
        out.extend(self.open_height.to_le_bytes());
        out.extend(encoded(&[self.return_address.as_bytes()])?);
        Ok(out)
    }
}
pub fn tab_id(
    keys: &PublicKeys,
    offer: &Offer,
    terms: &Terms,
    seal: &[u8; 32],
) -> Result<[u8; 32], RecordError> {
    terms.validate(offer)?;
    let parameters = encoded(&[&offer.canonical()?, &terms.canonical()?])?;
    encoding::hash(
        b"TidvarTabId",
        &encoded(&[&keys.group, &keys.viewing_key, &parameters, seal])?,
    )
    .map_err(|_| RecordError::Terms)
}
impl RecordBody {
    pub fn new(
        offer: Offer,
        terms: Terms,
        keys: &PublicKeys,
        seal_commitment: [u8; 32],
        challenge: Vec<u8>,
        payer_record_key: [u8; 32],
    ) -> Result<Self, RecordError> {
        let deadlines = terms.validate(&offer)?;
        let body = Self {
            tab_id: tab_id(keys, &offer, &terms, &seal_commitment)?,
            fee_reserve: Amount::try_from(params::FEE_RESERVE).map_err(|_| RecordError::Terms)?,
            maximum_payment: params::spendable(terms.funded).map_err(|_| RecordError::Terms)?,
            refund_ms: deadlines.refund_ms,
            refund_round: (deadlines.refund_ms / 1000)
                .checked_sub(1_692_803_367)
                .ok_or(RecordError::Terms)?
                / 3
                + 1,
            close_ms: deadlines.close_ms,
            expiry: params::iou_expiry(terms.open_height, terms.lifetime.seconds() * 1000)
                .map_err(|_| RecordError::Terms)?,
            group: keys.group,
            payer_share: keys.payer,
            service_share: keys.service,
            viewing_key: keys.viewing_key.clone(),
            offer,
            terms,
            seal_commitment,
            challenge,
            payer_record_key,
        };
        body.validate()?;
        Ok(body)
    }
    pub fn validate(&self) -> Result<(), RecordError> {
        let deadlines = self.terms.validate(&self.offer)?;
        if self.fee_reserve.value() != params::FEE_RESERVE
            || self.maximum_payment
                != params::spendable(self.terms.funded).map_err(|_| RecordError::Terms)?
            || self.refund_ms != deadlines.refund_ms
            || self.close_ms != deadlines.close_ms
            || self.refund_round
                != (self.refund_ms / 1000)
                    .checked_sub(1_692_803_367)
                    .ok_or(RecordError::Terms)?
                    / 3
                    + 1
            || self.expiry
                != params::iou_expiry(self.terms.open_height, self.terms.lifetime.seconds() * 1000)
                    .map_err(|_| RecordError::Terms)?
        {
            return Err(RecordError::Terms);
        }
        if self.challenge.len() != 32
            || self.challenge.iter().any(|i| !(1..=64).contains(i))
            || self
                .challenge
                .iter()
                .collect::<std::collections::BTreeSet<_>>()
                .len()
                != 32
        {
            return Err(RecordError::Terms);
        }
        if point(&self.group)?
            != point(&self.payer_share)? * pasta_curves::pallas::Scalar::from(2)
                - point(&self.service_share)?
        {
            return Err(RecordError::Terms);
        }
        let viewing: [u8; 96] = self
            .viewing_key
            .as_slice()
            .try_into()
            .map_err(|_| RecordError::Terms)?;
        if viewing[..32] != self.group
            || orchard::keys::FullViewingKey::from_bytes(&viewing).is_none()
        {
            return Err(RecordError::Terms);
        }
        let keys = PublicKeys {
            group: self.group,
            payer: self.payer_share,
            service: self.service_share,
            viewing_key: self.viewing_key.clone(),
            address: String::new(),
        };
        if tab_id(&keys, &self.offer, &self.terms, &self.seal_commitment)? != self.tab_id {
            return Err(RecordError::Terms);
        }
        point(&self.payer_record_key)?;
        Ok(())
    }
    pub fn canonical(&self) -> Result<Vec<u8>, RecordError> {
        self.validate()?;
        let mut out = self.tab_id.to_vec();
        out.extend(encoded(&[
            &self.offer.canonical()?,
            &self.terms.canonical()?,
        ])?);
        for value in [
            self.fee_reserve.value(),
            self.maximum_payment.value(),
            self.refund_ms,
            self.refund_round,
            self.close_ms,
        ] {
            out.extend(value.to_le_bytes());
        }
        out.extend(self.expiry.to_le_bytes());
        out.extend(self.group);
        out.extend(self.payer_share);
        out.extend(self.service_share);
        out.extend(encoded(&[&self.viewing_key])?);
        out.extend(self.seal_commitment);
        out.extend(encoded(&[&self.challenge])?);
        out.extend(self.payer_record_key);
        Ok(out)
    }
    pub fn hash(&self) -> Result<[u8; 32], RecordError> {
        encoding::hash(b"TidvarRecord", &self.canonical()?).map_err(|_| RecordError::Terms)
    }
}
fn verify(public: [u8; 32], signature: &[u8], message: &[u8]) -> Result<(), RecordError> {
    let signature: [u8; 64] = signature.try_into().map_err(|_| RecordError::Signature)?;
    VerificationKey::<SpendAuth>::try_from(public)
        .map_err(|_| RecordError::Signature)?
        .verify(message, &Signature::from(signature))
        .map_err(|_| RecordError::Signature)
}
impl ServiceSigned {
    pub fn sign<R: RngCore + CryptoRng>(
        body: RecordBody,
        key: &SigningKey<SpendAuth>,
        rng: &mut R,
    ) -> Result<Self, RecordError> {
        let public: [u8; 32] = VerificationKey::from(key).into();
        if public != body.offer.service_key {
            return Err(RecordError::Signature);
        }
        let signature: [u8; 64] = key.sign(rng, &body.hash()?).into();
        Ok(Self {
            body,
            signature: signature.to_vec(),
        })
    }
    pub fn verify(&self) -> Result<(), RecordError> {
        verify(
            self.body.offer.service_key,
            &self.signature,
            &self.body.hash()?,
        )
    }
    pub fn countersign<R: RngCore + CryptoRng>(
        self,
        key: &SigningKey<SpendAuth>,
        rng: &mut R,
    ) -> Result<SignedRecord, RecordError> {
        self.verify()?;
        let public: [u8; 32] = VerificationKey::from(key).into();
        if public != self.body.payer_record_key {
            return Err(RecordError::Signature);
        }
        let signature: [u8; 64] = key.sign(rng, &self.body.hash()?).into();
        Ok(SignedRecord {
            body: self.body,
            service_signature: self.signature,
            payer_signature: signature.to_vec(),
        })
    }
}
impl SignedRecord {
    pub fn verify(&self) -> Result<(), RecordError> {
        let hash = self.body.hash()?;
        verify(self.body.offer.service_key, &self.service_signature, &hash)?;
        verify(self.body.payer_record_key, &self.payer_signature, &hash)
    }
}

#[cfg(test)]
mod network_tests {
    use super::*;

    #[test]
    fn each_network_signs_for_its_own_ironwood_branch() {
        // Mainnet: Ironwood from NU6.3 (block 3,428,143); NU7 not yet scheduled.
        assert_eq!(Network::Mainnet.ironwood_branch(3_428_142), None);
        assert_eq!(
            Network::Mainnet.ironwood_branch(3_428_143),
            Some(BranchId::Nu6_3)
        );
        assert_eq!(
            Network::Mainnet.ironwood_branch(3_600_000),
            Some(BranchId::Nu6_3)
        );
        // Testnet already runs NU7 (block 4,465,026).
        assert_eq!(
            Network::Testnet.ironwood_branch(4_465_025),
            Some(BranchId::Nu6_3)
        );
        assert_eq!(
            Network::Testnet.ironwood_branch(4_465_026),
            Some(BranchId::Nu7)
        );
        assert_eq!(Network::Mainnet.coin_type(), 133);
        assert_eq!(Network::Testnet.coin_type(), 1);
        assert_eq!(Network::from_name("mainnet"), Some(Network::Mainnet));
        assert_eq!(Network::from_name("test"), Some(Network::Testnet));
        assert_eq!(Network::from_name("regtest"), None);
    }

    #[test]
    fn mainnet_switches_to_nu7_when_a_server_reports_it() {
        // Testnet branch ids are ignored for mainnet, and NU6.3 reports change nothing.
        observe_branch(Network::Testnet, "77190ad9", 3_900_000);
        observe_branch(Network::Mainnet, "37a5165b", 3_900_000);
        assert_eq!(
            Network::Mainnet.ironwood_branch(3_900_001),
            Some(BranchId::Nu6_3)
        );
        observe_branch(Network::Mainnet, "77190ad9", 3_900_000);
        assert_eq!(
            Network::Mainnet.ironwood_branch(3_900_001),
            Some(BranchId::Nu7)
        );
        assert_eq!(
            Network::Mainnet.ironwood_branch(3_899_999),
            Some(BranchId::Nu6_3)
        );
        // A later report never moves the activation forward.
        observe_branch(Network::Mainnet, "77190ad9", 3_950_000);
        assert_eq!(
            Network::Mainnet.ironwood_branch(3_900_001),
            Some(BranchId::Nu7)
        );
    }

    #[test]
    fn fifteen_minute_tabs_keep_their_deadline_order() -> Result<(), crate::params::AmountError> {
        let d = Deadlines::new(1_000, Lifetime::Minutes15.seconds() * 1000)?;
        assert!(1_000 < d.pack_ms && d.pack_ms < d.watch_ms && d.watch_ms < d.close_ms);
        assert_eq!(d.refund_ms - d.close_ms, 450_000);
        Ok(())
    }
}
