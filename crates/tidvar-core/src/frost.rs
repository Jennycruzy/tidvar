use crate::{
    encoding::{self, encode_bytes},
    keys::{Keys, Role},
};
use ff::PrimeField;
use group::{Group, GroupEncoding};
use pasta_curves::pallas::{Point, Scalar};
use rand_core::{CryptoRng, RngCore};
use reddsa::frost::redpallas as frost;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;
use zeroize::Zeroizing;

#[derive(Debug, Error)]
pub enum SigningError {
    #[error("The signing values could not be verified. No payment was accepted.")]
    Nonce,
    #[error("The payment signature could not be verified. No payment was accepted.")]
    Share,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Commitment {
    #[serde(with = "crate::encoding::fixed32")]
    pub id: [u8; 32],
    #[serde(with = "crate::encoding::bytes")]
    pub data: Vec<u8>,
}
pub struct Nonce {
    private: frost::round1::SigningNonces,
    public: Commitment,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message {
    #[serde(with = "crate::encoding::fixed32")]
    pub sighash: [u8; 32],
    #[serde(with = "crate::encoding::fixed32")]
    pub alpha: [u8; 32],
    pub payer: Commitment,
    pub service: Commitment,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Share {
    pub role: Role,
    #[serde(with = "crate::encoding::fixed32")]
    pub value: [u8; 32],
}
fn bytes32(bytes: Vec<u8>) -> Result<[u8; 32], SigningError> {
    bytes.try_into().map_err(|_| SigningError::Share)
}
fn scalar(bytes: &[u8; 32]) -> Result<Scalar, SigningError> {
    Option::from(Scalar::from_repr(*bytes)).ok_or(SigningError::Share)
}
fn point(bytes: &[u8; 32]) -> Result<Point, SigningError> {
    Option::<Point>::from(Point::from_bytes(bytes))
        .filter(|p| !bool::from(p.is_identity()))
        .ok_or(SigningError::Share)
}
fn basepoint() -> Result<Point, SigningError> {
    let mut one = [0; 32];
    one[0] = 1;
    let key = reddsa::SigningKey::<reddsa::orchard::SpendAuth>::try_from(one)
        .map_err(|_| SigningError::Share)?;
    point(&reddsa::VerificationKey::from(&key).into())
}
fn identifier(role: Role) -> Result<frost::Identifier, SigningError> {
    role.identifier().map_err(|_| SigningError::Share)
}
impl Commitment {
    fn from_public(value: &frost::round1::SigningCommitments) -> Result<Self, SigningError> {
        let data = value.serialize().map_err(|_| SigningError::Nonce)?;
        let id = encoding::hash(
            b"TidvarNonceId",
            &encode_bytes(&[&data]).map_err(|_| SigningError::Nonce)?,
        )
        .map_err(|_| SigningError::Nonce)?;
        Ok(Self { id, data })
    }
    pub fn verify(&self) -> Result<frost::round1::SigningCommitments, SigningError> {
        let parsed = frost::round1::SigningCommitments::deserialize(&self.data)
            .map_err(|_| SigningError::Nonce)?;
        let expected = Self::from_public(&parsed)?;
        if expected.data != self.data || expected.id != self.id {
            return Err(SigningError::Nonce);
        }
        Ok(parsed)
    }
}
impl Nonce {
    pub fn generate<R: RngCore + CryptoRng>(
        keys: &Keys,
        rng: &mut R,
    ) -> Result<Self, SigningError> {
        let (private, public) = frost::round1::commit(keys.package.signing_share(), rng);
        Ok(Self {
            private,
            public: Commitment::from_public(&public)?,
        })
    }
    pub fn public(&self) -> &Commitment {
        &self.public
    }
    pub fn secret_bytes(&self) -> Result<Zeroizing<Vec<u8>>, SigningError> {
        Ok(Zeroizing::new(
            self.private.serialize().map_err(|_| SigningError::Nonce)?,
        ))
    }
    pub fn restore(secret: &[u8], public: Commitment) -> Result<Self, SigningError> {
        if secret.len() > encoding::MAX_MESSAGE_BYTES {
            return Err(SigningError::Nonce);
        }
        let private =
            frost::round1::SigningNonces::deserialize(secret).map_err(|_| SigningError::Nonce)?;
        let private =
            frost::round1::SigningNonces::from_nonces(*private.hiding(), *private.binding());
        let expected = Commitment::from_public(&frost::round1::SigningCommitments::from(&private))?;
        public.verify()?;
        if expected.data != public.data || expected.id != public.id {
            return Err(SigningError::Nonce);
        }
        Ok(Self { private, public })
    }
    pub fn sign(self, keys: &Keys, message: &Message) -> Result<Share, SigningError> {
        let assigned = match keys.role {
            Role::Payer => &message.payer,
            Role::Service => &message.service,
        };
        if assigned.id != self.public.id || assigned.data != self.public.data {
            return Err(SigningError::Nonce);
        }
        let public = randomized_public(keys, &message.alpha)?;
        let original = Zeroizing::new(bytes32(keys.package.signing_share().serialize())?);
        let secret = Zeroizing::new(scalar(&original)?);
        let randomized = Zeroizing::new((*secret + scalar(&message.alpha)?).to_repr());
        let signing = frost::keys::SigningShare::deserialize(randomized.as_ref())
            .map_err(|_| SigningError::Share)?;
        let verifying = public
            .verifying_shares()
            .get(&identifier(keys.role)?)
            .ok_or(SigningError::Share)?;
        let key = frost::keys::KeyPackage::new(
            identifier(keys.role)?,
            signing,
            *verifying,
            *public.verifying_key(),
            2,
        );
        let share = frost::round2::sign(&message.package()?, &self.private, &key)
            .map_err(|_| SigningError::Share)?;
        Ok(Share {
            role: keys.role,
            value: bytes32(share.serialize())?,
        })
    }
}
impl Message {
    pub fn package(&self) -> Result<frost::SigningPackage, SigningError> {
        scalar(&self.alpha)?;
        if self.payer.id == self.service.id {
            return Err(SigningError::Nonce);
        }
        Ok(frost::SigningPackage::new(
            BTreeMap::from([
                (identifier(Role::Payer)?, self.payer.verify()?),
                (identifier(Role::Service)?, self.service.verify()?),
            ]),
            &self.sighash,
        ))
    }
    pub fn hash(&self) -> Result<[u8; 32], SigningError> {
        self.package()?;
        encoding::hash(
            b"TidvarSignMsg",
            &encode_bytes(&[
                &self.sighash,
                &self.alpha,
                &self.payer.data,
                &self.service.data,
            ])
            .map_err(|_| SigningError::Nonce)?,
        )
        .map_err(|_| SigningError::Nonce)
    }
}
fn randomized_public(
    keys: &Keys,
    alpha: &[u8; 32],
) -> Result<frost::keys::PublicKeyPackage, SigningError> {
    let offset = basepoint()? * scalar(alpha)?;
    let mut shares = BTreeMap::new();
    for (role, bytes) in [
        (Role::Payer, keys.public.payer),
        (Role::Service, keys.public.service),
    ] {
        let randomized = (point(&bytes)? + offset).to_bytes();
        shares.insert(
            identifier(role)?,
            frost::keys::VerifyingShare::deserialize(&randomized)
                .map_err(|_| SigningError::Share)?,
        );
    }
    let group = (point(&keys.public.group)? + offset).to_bytes();
    let group = frost::VerifyingKey::deserialize(&group).map_err(|_| SigningError::Share)?;
    Ok(frost::keys::PublicKeyPackage::new(shares, group, Some(2)))
}
pub fn verify_share(keys: &Keys, message: &Message, share: &Share) -> Result<(), SigningError> {
    let public = randomized_public(keys, &message.alpha)?;
    let id = identifier(share.role)?;
    let verifying = public
        .verifying_shares()
        .get(&id)
        .ok_or(SigningError::Share)?;
    let signature = frost::round2::SignatureShare::deserialize(&share.value)
        .map_err(|_| SigningError::Share)?;
    frost::frost::verify_signature_share(
        id,
        verifying,
        &signature,
        &message.package()?,
        public.verifying_key(),
    )
    .map_err(|_| SigningError::Share)
}
pub fn aggregate(
    keys: &Keys,
    message: &Message,
    payer: &Share,
    service: &Share,
) -> Result<[u8; 64], SigningError> {
    if payer.role != Role::Payer || service.role != Role::Service {
        return Err(SigningError::Share);
    }
    verify_share(keys, message, payer)?;
    verify_share(keys, message, service)?;
    let public = randomized_public(keys, &message.alpha)?;
    let shares = BTreeMap::from([
        (
            identifier(Role::Payer)?,
            frost::round2::SignatureShare::deserialize(&payer.value)
                .map_err(|_| SigningError::Share)?,
        ),
        (
            identifier(Role::Service)?,
            frost::round2::SignatureShare::deserialize(&service.value)
                .map_err(|_| SigningError::Share)?,
        ),
    ]);
    let signature =
        frost::aggregate(&message.package()?, &shares, &public).map_err(|_| SigningError::Share)?;
    signature
        .serialize()
        .map_err(|_| SigningError::Share)?
        .try_into()
        .map_err(|_| SigningError::Share)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keys::FirstRound;
    use rand_core::OsRng;
    use zcash_protocol::consensus::NetworkType;
    fn random_scalar() -> Scalar {
        loop {
            let mut bytes = [0; 32];
            OsRng.fill_bytes(&mut bytes);
            if let Some(s) = Option::<Scalar>::from(Scalar::from_repr(bytes)) {
                return s;
            }
        }
    }
    #[test]
    fn service_checks_payer_before_consuming_nonce_and_sdk_verifies_final_signature()
    -> Result<(), Box<dyn std::error::Error>> {
        let (p, p1) = FirstRound::begin(Role::Payer, &mut OsRng)?;
        let (s, s1) = FirstRound::begin(Role::Service, &mut OsRng)?;
        let (p, p2) = p.advance(s1)?;
        let (s, s2) = s.advance(p1)?;
        let p = p.finish(s2, NetworkType::Test)?;
        let s = s.finish(p2, NetworkType::Test)?;
        let pn = Nonce::generate(&p, &mut OsRng)?;
        let sn = Nonce::generate(&s, &mut OsRng)?;
        let saved = sn.secret_bytes()?;
        let service = sn.public().clone();
        let message = Message {
            sighash: encoding::hash(b"TidvarSignTest", &p.public_bytes()?)?,
            alpha: random_scalar().to_repr(),
            payer: pn.public().clone(),
            service: service.clone(),
        };
        let ps = pn.sign(&p, &message)?;
        verify_share(&s, &message, &ps)?;
        assert_eq!(*saved, *sn.secret_bytes()?);
        let mut changed = message.clone();
        changed.sighash[0] ^= 1;
        assert!(verify_share(&s, &changed, &ps).is_err());
        let mut changed = message.clone();
        changed.alpha = random_scalar().to_repr();
        assert!(verify_share(&s, &changed, &ps).is_err());
        let mut renamed = service.clone();
        renamed.id[0] ^= 1;
        assert!(renamed.verify().is_err());
        drop(sn);
        let restored = Nonce::restore(&saved, service)?;
        let ss = restored.sign(&s, &message)?;
        let signature = aggregate(&s, &message, &ps, &ss)?;
        let key = bytes32(
            randomized_public(&s, &message.alpha)?
                .verifying_key()
                .serialize()?,
        )?;
        let key = orchard::primitives::redpallas::VerificationKey::<
            orchard::primitives::redpallas::SpendAuth,
        >::try_from(key)
        .map_err(|_| SigningError::Share)?;
        key.verify(
            &message.sighash,
            &orchard::primitives::redpallas::Signature::from(signature),
        )
        .map_err(|_| SigningError::Share)?;
        Ok(())
    }
}
