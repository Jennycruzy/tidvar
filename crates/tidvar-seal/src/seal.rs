use crate::{SealError, VerifiedBeacon, ibe};
use chacha20::cipher::{KeyIvInit, StreamCipher};
use ff::{Field, PrimeField};
use group::{Group, GroupEncoding};
use pasta_curves::pallas::{Point, Scalar};
use rand_core::{CryptoRng, RngCore};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use tidvar_core::encoding::{self, encode_bytes};
use zeroize::{Zeroize, Zeroizing};

pub const SHARE_COUNT: usize = 64;
pub const THRESHOLD: usize = 33;
pub const OPEN_COUNT: usize = 32;

#[derive(Clone, Serialize, Deserialize)]
pub struct PointBytes(#[serde(with = "tidvar_core::encoding::fixed32")] pub [u8; 32]);

#[derive(Clone, Serialize, Deserialize)]
pub struct SealedShare {
    #[serde(with = "tidvar_core::encoding::bytes")]
    pub time_key: Vec<u8>,
    #[serde(with = "tidvar_core::encoding::fixed32")]
    pub encrypted_share: [u8; 32],
}

#[derive(Clone, Serialize, Deserialize)]
pub struct SealedKey {
    pub round: u64,
    pub coefficients: Vec<PointBytes>,
    pub shares: Vec<SealedShare>,
}

#[derive(Clone, Serialize, Deserialize, Zeroize)]
pub struct Opening {
    pub index: u8,
    #[serde(with = "tidvar_core::encoding::fixed32")]
    pub share: [u8; 32],
    #[serde(with = "tidvar_core::encoding::fixed16")]
    pub key: [u8; 16],
    #[serde(with = "tidvar_core::encoding::fixed16")]
    pub sigma: [u8; 16],
}

#[derive(Clone, Serialize, Deserialize)]
pub struct Challenge {
    #[serde(with = "tidvar_core::encoding::fixed32")]
    pub commitment: [u8; 32],
    pub indices: Vec<u8>,
}

pub struct SecretSeal {
    pub sealed: SealedKey,
    openings: Zeroizing<Vec<Opening>>,
    answered: Option<BTreeSet<u8>>,
}

pub struct Verifier {
    sealed: SealedKey,
    commitment: [u8; 32],
    public_key: Vec<u8>,
    base: Point,
    expected: Vec<Point>,
}

pub struct VerifiedSeal {
    verifier: Verifier,
    challenge: Challenge,
    openings: Vec<Opening>,
}

fn decode_scalar(bytes: [u8; 32]) -> Result<Scalar, SealError> {
    Option::<Scalar>::from(Scalar::from_repr(bytes)).ok_or(SealError::Invalid)
}
fn decode_point(bytes: &[u8; 32]) -> Result<Point, SealError> {
    Option::<Point>::from(Point::from_bytes(bytes)).ok_or(SealError::Invalid)
}
fn random_scalar<R: RngCore + CryptoRng>(rng: &mut R) -> Scalar {
    loop {
        let mut bytes = [0; 32];
        rng.fill_bytes(&mut bytes);
        if let Some(value) = Option::<Scalar>::from(Scalar::from_repr(bytes)) {
            bytes.zeroize();
            return value;
        }
        bytes.zeroize();
    }
}
fn basepoint() -> Result<Point, SealError> {
    let key = reddsa::SigningKey::<reddsa::orchard::SpendAuth>::try_from(Scalar::ONE.to_repr())
        .map_err(|_| SealError::Invalid)?;
    let point: [u8; 32] = reddsa::VerificationKey::from(&key).into();
    decode_point(&point)
}
fn mask(key: &[u8; 16], mut share: [u8; 32]) -> Result<[u8; 32], SealError> {
    let derived = Zeroizing::new(encoding::hash(b"TidvarSealKey", &encode_bytes(&[key])?)?);
    let mut cipher = chacha20::ChaCha20::new((&*derived).into(), (&[0; 12]).into());
    cipher.apply_keystream(&mut share);
    Ok(share)
}
fn indices(values: &[u8]) -> Result<BTreeSet<u8>, SealError> {
    let unique: BTreeSet<_> = values.iter().copied().collect();
    if values.len() != OPEN_COUNT
        || unique.len() != OPEN_COUNT
        || unique.iter().any(|i| !(1..=64).contains(i))
    {
        return Err(SealError::Invalid);
    }
    Ok(unique)
}

impl SealedKey {
    pub fn commitment(&self) -> Result<[u8; 32], SealError> {
        if self.coefficients.len() != THRESHOLD
            || self.shares.len() != SHARE_COUNT
            || self.shares.iter().any(|s| s.time_key.len() != 128)
        {
            return Err(SealError::Invalid);
        }
        let mut encoded = self.round.to_le_bytes().to_vec();
        encoded.extend_from_slice(&(THRESHOLD as u32).to_le_bytes());
        for coefficient in &self.coefficients {
            encoded.extend_from_slice(&encode_bytes(&[&coefficient.0])?);
        }
        encoded.extend_from_slice(&(SHARE_COUNT as u32).to_le_bytes());
        for share in &self.shares {
            encoded.extend_from_slice(&encode_bytes(&[&share.time_key, &share.encrypted_share])?);
        }
        Ok(encoding::hash(b"TidvarSealCommit", &encoded)?)
    }
}

impl SecretSeal {
    pub fn create<R: RngCore + CryptoRng>(
        secret: &[u8; 32],
        public_key: &[u8],
        round: u64,
        rng: &mut R,
    ) -> Result<Self, SealError> {
        let base = basepoint()?;
        let mut coefficients = Zeroizing::new(vec![decode_scalar(*secret)?]);
        for _ in 1..THRESHOLD {
            coefficients.push(random_scalar(rng));
        }
        if coefficients.last().is_some_and(|v| bool::from(v.is_zero())) {
            return Err(SealError::Invalid);
        }
        let commitments = coefficients
            .iter()
            .map(|a| PointBytes((base * a).to_bytes()))
            .collect();
        let mut shares = Vec::with_capacity(SHARE_COUNT);
        let mut openings = Zeroizing::new(Vec::with_capacity(SHARE_COUNT));
        for index in 1..=SHARE_COUNT {
            let x = Scalar::from(index as u64);
            let mut value = coefficients
                .iter()
                .rev()
                .fold(Scalar::ZERO, |sum, a| sum * x + a);
            let mut key = [0; 16];
            let mut sigma = [0; 16];
            rng.fill_bytes(&mut key);
            rng.fill_bytes(&mut sigma);
            shares.push(SealedShare {
                time_key: ibe::encrypt_with_sigma(public_key, round, &key, &sigma)?,
                encrypted_share: mask(&key, value.to_repr())?,
            });
            openings.push(Opening {
                index: index as u8,
                share: value.to_repr(),
                key,
                sigma,
            });
            key.zeroize();
            sigma.zeroize();
            value.zeroize();
        }
        Ok(Self {
            sealed: SealedKey {
                round,
                coefficients: commitments,
                shares,
            },
            openings,
            answered: None,
        })
    }

    pub fn answer(&mut self, challenge: &Challenge) -> Result<Vec<Opening>, SealError> {
        if challenge.commitment != self.sealed.commitment()? {
            return Err(SealError::Invalid);
        }
        let selected = indices(&challenge.indices)?;
        if self
            .answered
            .as_ref()
            .is_some_and(|previous| previous != &selected)
        {
            return Err(SealError::Invalid);
        }
        self.answered = Some(selected);
        challenge
            .indices
            .iter()
            .map(|i| {
                self.openings
                    .get(usize::from(*i) - 1)
                    .cloned()
                    .ok_or(SealError::Invalid)
            })
            .collect()
    }
}

impl Verifier {
    pub fn accept(
        sealed: SealedKey,
        commitment: [u8; 32],
        expected_service: &[u8; 32],
        expected_round: u64,
        public_key: &[u8],
    ) -> Result<Self, SealError> {
        if sealed.round != expected_round || sealed.commitment()? != commitment {
            return Err(SealError::Invalid);
        }
        let coefficients = sealed
            .coefficients
            .iter()
            .map(|p| decode_point(&p.0))
            .collect::<Result<Vec<_>, _>>()?;
        let service = decode_point(expected_service)?;
        if bool::from(service.is_identity()) || coefficients[0] != service {
            return Err(SealError::Invalid);
        }
        let expected = (1..=SHARE_COUNT)
            .map(|index| {
                let x = Scalar::from(index as u64);
                coefficients
                    .iter()
                    .rev()
                    .fold(Point::identity(), |sum, p| sum * x + p)
            })
            .collect();
        Ok(Self {
            sealed,
            commitment,
            public_key: public_key.to_vec(),
            base: basepoint()?,
            expected,
        })
    }

    pub fn challenge<R: RngCore + CryptoRng>(&self, rng: &mut R) -> Challenge {
        let mut pool: Vec<_> = (1..=SHARE_COUNT as u8).collect();
        for offset in 0..OPEN_COUNT {
            let remaining = (SHARE_COUNT - offset) as u64;
            let limit = u64::MAX - u64::MAX % remaining;
            let value = loop {
                let value = rng.next_u64();
                if value < limit {
                    break value;
                }
            };
            pool.swap(offset, offset + (value % remaining) as usize);
        }
        pool.truncate(OPEN_COUNT);
        Challenge {
            commitment: self.commitment,
            indices: pool,
        }
    }

    pub fn verify_opening(&self, opening: &Opening) -> Result<(), SealError> {
        if !(1..=64).contains(&opening.index) {
            return Err(SealError::Invalid);
        }
        let position = usize::from(opening.index) - 1;
        let value = decode_scalar(opening.share)?;
        if self.base * value != self.expected[position]
            || ibe::encrypt_with_sigma(
                &self.public_key,
                self.sealed.round,
                &opening.key,
                &opening.sigma,
            )? != self.sealed.shares[position].time_key
            || mask(&opening.key, opening.share)? != self.sealed.shares[position].encrypted_share
        {
            return Err(SealError::Invalid);
        }
        Ok(())
    }

    pub fn finish(
        self,
        challenge: Challenge,
        openings: Vec<Opening>,
    ) -> Result<VerifiedSeal, SealError> {
        if challenge.commitment != self.commitment {
            return Err(SealError::Invalid);
        }
        let chosen = indices(&challenge.indices)?;
        if openings.len() != OPEN_COUNT
            || openings.iter().map(|o| o.index).collect::<BTreeSet<_>>() != chosen
        {
            return Err(SealError::Invalid);
        }
        for opening in &openings {
            self.verify_opening(opening)?;
        }
        Ok(VerifiedSeal {
            verifier: self,
            challenge,
            openings,
        })
    }
}

impl VerifiedSeal {
    pub fn sealed(&self) -> &SealedKey {
        &self.verifier.sealed
    }
    pub fn challenge(&self) -> &Challenge {
        &self.challenge
    }
    pub fn openings(&self) -> &[Opening] {
        &self.openings
    }
    pub fn open(&self, beacon: &VerifiedBeacon) -> Result<Zeroizing<[u8; 32]>, SealError> {
        if beacon.round() != self.verifier.sealed.round {
            return Err(SealError::Beacon);
        }
        let mut valid: Vec<_> = self
            .openings
            .iter()
            .map(|o| Ok((o.index, decode_scalar(o.share)?)))
            .collect::<Result<_, SealError>>()?;
        for (position, sealed) in self.verifier.sealed.shares.iter().enumerate() {
            let index = (position + 1) as u8;
            if self.challenge.indices.contains(&index) {
                continue;
            }
            if let Ok((key, _)) = ibe::decrypt_exact(beacon.signature(), &sealed.time_key)
                && let Ok(value) = mask(&key, sealed.encrypted_share).and_then(decode_scalar)
                && self.verifier.base * value == self.verifier.expected[position]
            {
                valid.push((index, value));
            }
        }
        if valid.len() < THRESHOLD {
            return Err(SealError::Invalid);
        }
        valid.truncate(THRESHOLD);
        let mut secret = Scalar::ZERO;
        for (index, value) in &valid {
            let x = Scalar::from(u64::from(*index));
            let mut numerator = Scalar::ONE;
            let mut denominator = Scalar::ONE;
            for (other, _) in &valid {
                if other == index {
                    continue;
                }
                let y = Scalar::from(u64::from(*other));
                numerator *= -y;
                denominator *= x - y;
            }
            let inverse = Option::<Scalar>::from(denominator.invert()).ok_or(SealError::Invalid)?;
            secret += value * numerator * inverse;
        }
        if self.verifier.base * secret != self.verifier.expected_service()? {
            return Err(SealError::Invalid);
        }
        let result = Zeroizing::new(secret.to_repr());
        secret.zeroize();
        Ok(result)
    }
}

impl Verifier {
    fn expected_service(&self) -> Result<Point, SealError> {
        decode_point(&self.sealed.coefficients[0].0)
    }
}
