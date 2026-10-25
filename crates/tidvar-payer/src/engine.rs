use crate::root::{Root, RootError};
use rand_core::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tidvar_chain::sync::{Snapshot, SyncState};
use tidvar_core::{
    encoding,
    keys::{FirstRound, KeyError, Keys, Role, RoundOne, RoundTwo, SecondRound},
};
use tidvar_seal::{
    SealError, quicknet_public_key,
    seal::{Challenge, Opening, SealedKey, VerifiedSeal, Verifier},
};
use zcash_protocol::consensus::NetworkType;
use zeroize::{Zeroize, ZeroizeOnDrop, Zeroizing};

#[derive(Debug, Error)]
pub enum PayerError {
    #[error("The saved tab could not be verified. Keep your backup and try again.")]
    Backup,
    #[error("This payment commitment has already been used.")]
    ReusedCommitment,
    #[error(transparent)]
    Keys(#[from] KeyError),
    #[error(transparent)]
    Seal(#[from] SealError),
    #[error(transparent)]
    Root(#[from] RootError),
    #[error(transparent)]
    Chain(#[from] tidvar_chain::ChainError),
}

pub struct Setup {
    inner: FirstRound,
}
pub struct SetupSecond {
    inner: SecondRound,
}
pub struct UnsealedPayer {
    keys: Keys,
}
pub struct PendingPayer {
    keys: Keys,
    verifier: Verifier,
    challenge: Challenge,
}
pub struct Payer {
    keys: Keys,
    seal: VerifiedSeal,
    sync: Option<SyncState>,
    consumed: Vec<[u8; 32]>,
}

#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
struct SecretBytes(#[serde(with = "tidvar_core::encoding::bytes")] Vec<u8>);
#[derive(Serialize, Deserialize, Zeroize, ZeroizeOnDrop)]
struct SavedOpenings(Vec<Opening>);
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Backup {
    version: u32,
    package: SecretBytes,
    #[serde(with = "tidvar_core::encoding::bytes")]
    public_package: Vec<u8>,
    #[serde(with = "tidvar_core::encoding::bytes")]
    viewing_key: Vec<u8>,
    sealed: SealedKey,
    challenge: Challenge,
    openings: SavedOpenings,
    sync: Option<Snapshot>,
    consumed: Vec<[u8; 32]>,
    #[serde(with = "tidvar_core::encoding::fixed32")]
    owner: [u8; 32],
}

impl Setup {
    pub fn begin<R: RngCore + CryptoRng>(rng: &mut R) -> Result<(Self, RoundOne), PayerError> {
        let (inner, message) = FirstRound::begin(Role::Payer, rng)?;
        Ok((Self { inner }, message))
    }
    pub fn advance(self, peer: RoundOne) -> Result<(SetupSecond, RoundTwo), PayerError> {
        let (inner, message) = self.inner.advance(peer)?;
        Ok((SetupSecond { inner }, message))
    }
}
impl SetupSecond {
    pub fn finish(self, peer: RoundTwo) -> Result<UnsealedPayer, PayerError> {
        Ok(UnsealedPayer {
            keys: self.inner.finish(peer, NetworkType::Test)?,
        })
    }
}
impl UnsealedPayer {
    pub fn public_keys(&self) -> &tidvar_core::keys::PublicKeys {
        &self.keys.public
    }
    pub fn accept<R: RngCore + CryptoRng>(
        self,
        sealed: SealedKey,
        commitment: [u8; 32],
        round: u64,
        rng: &mut R,
    ) -> Result<(PendingPayer, Challenge), PayerError> {
        let verifier = Verifier::accept(
            sealed,
            commitment,
            &self.keys.public.service,
            round,
            &quicknet_public_key()?,
        )?;
        let challenge = verifier.challenge(rng);
        Ok((
            PendingPayer {
                keys: self.keys,
                verifier,
                challenge: challenge.clone(),
            },
            challenge,
        ))
    }
}
impl PendingPayer {
    pub fn finish(self, openings: Vec<Opening>) -> Result<Payer, PayerError> {
        let seal = self.verifier.finish(self.challenge, openings)?;
        Ok(Payer {
            keys: self.keys,
            seal,
            sync: None,
            consumed: Vec::new(),
        })
    }
}
impl Payer {
    pub fn public_keys(&self) -> &tidvar_core::keys::PublicKeys {
        &self.keys.public
    }
    pub fn seal(&self) -> &VerifiedSeal {
        &self.seal
    }
    pub fn sync(&self) -> Option<&SyncState> {
        self.sync.as_ref()
    }
    pub fn sync_mut(&mut self) -> Option<&mut SyncState> {
        self.sync.as_mut()
    }
    pub fn start_sync(
        &mut self,
        before: tidvar_chain::service::TreeState,
    ) -> Result<(), PayerError> {
        if self.sync.is_some() {
            return Err(PayerError::Backup);
        }
        self.sync = Some(SyncState::new(&self.keys.public.viewing_key, before)?);
        Ok(())
    }
    pub fn consume_commitment(&mut self, bytes: &[u8]) -> Result<(), PayerError> {
        let parsed = reddsa::frost::redpallas::round1::SigningCommitments::deserialize(bytes)
            .map_err(|_| PayerError::Backup)?;
        let canonical = parsed.serialize().map_err(|_| PayerError::Backup)?;
        let hash = encoding::hash(
            b"TidvarUsedNonce",
            &encoding::encode_bytes(&[&canonical]).map_err(|_| PayerError::Backup)?,
        )
        .map_err(|_| PayerError::Backup)?;
        if self.consumed.contains(&hash) {
            return Err(PayerError::ReusedCommitment);
        }
        self.consumed.push(hash);
        Ok(())
    }
    pub fn backup(&self, root: &Root) -> Result<Zeroizing<Vec<u8>>, PayerError> {
        let record_key = root.signing_key(&self.seal.sealed().commitment()?)?;
        let backup = Backup {
            version: 1,
            package: SecretBytes(self.keys.secret_bytes()?.to_vec()),
            public_package: self.keys.public_bytes()?,
            viewing_key: self.keys.public.viewing_key.clone(),
            sealed: self.seal.sealed().clone(),
            challenge: self.seal.challenge().clone(),
            openings: SavedOpenings(self.seal.openings().to_vec()),
            sync: self.sync.as_ref().map(SyncState::snapshot).transpose()?,
            consumed: self.consumed.clone(),
            owner: reddsa::VerificationKey::from(&record_key).into(),
        };
        Ok(Zeroizing::new(
            serde_json::to_vec(&backup).map_err(|_| PayerError::Backup)?,
        ))
    }
    pub fn restore(root: &Root, plaintext: &[u8]) -> Result<Self, PayerError> {
        if plaintext.len() > 16 * 1024 * 1024 {
            return Err(PayerError::Backup);
        }
        let backup: Backup = serde_json::from_slice(plaintext).map_err(|_| PayerError::Backup)?;
        if backup.version != 1 {
            return Err(PayerError::Backup);
        }
        let keys = Keys::restore(
            Role::Payer,
            &backup.package.0,
            &backup.public_package,
            &backup.viewing_key,
            NetworkType::Test,
        )?;
        let commitment = backup.sealed.commitment()?;
        let owner: [u8; 32] = reddsa::VerificationKey::from(&root.signing_key(&commitment)?).into();
        if owner != backup.owner {
            return Err(PayerError::Backup);
        }
        let verifier = Verifier::accept(
            backup.sealed.clone(),
            commitment,
            &keys.public.service,
            backup.sealed.round,
            &quicknet_public_key()?,
        )?;
        let seal = verifier.finish(backup.challenge, backup.openings.0.clone())?;
        let sync = backup.sync.map(SyncState::restore).transpose()?;
        if sync.as_ref().is_some_and(|state| {
            state.viewing_key().to_bytes().as_slice() != keys.public.viewing_key
        }) {
            return Err(PayerError::Backup);
        }
        let unique: std::collections::BTreeSet<_> = backup.consumed.iter().collect();
        if unique.len() != backup.consumed.len() {
            return Err(PayerError::Backup);
        }
        Ok(Self {
            keys,
            seal,
            sync,
            consumed: backup.consumed,
        })
    }
}
