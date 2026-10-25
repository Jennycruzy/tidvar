use crate::encoding::{self, EncodingError};
use group::{Group, GroupEncoding};
use pasta_curves::pallas::Point;
use rand_core::{CryptoRng, RngCore};
use reddsa::frost::redpallas::{
    self as frost,
    keys::{EvenY, dkg},
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use thiserror::Error;
use zcash_address::unified::{self, Encoding};
use zcash_protocol::consensus::NetworkType;
use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Role {
    Payer,
    Service,
}
impl Role {
    pub fn identifier(self) -> Result<frost::Identifier, KeyError> {
        frost::Identifier::try_from(match self {
            Self::Payer => 1u16,
            Self::Service => 2u16,
        })
        .map_err(|_| KeyError::Message)
    }
    fn peer(self) -> Self {
        match self {
            Self::Payer => Self::Service,
            Self::Service => Self::Payer,
        }
    }
}

#[derive(Debug, Error)]
pub enum KeyError {
    #[error("Setting up the lock failed. No money moved. Try again.")]
    Message,
    #[error("The other party changed its viewing-key contribution.")]
    Commitment,
    #[error("The joint key failed its verification checks. Start a new lock.")]
    Group,
    #[error(transparent)]
    Encoding(#[from] EncodingError),
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RoundOne {
    pub role: Role,
    #[serde(with = "crate::encoding::bytes")]
    pub package: Vec<u8>,
    #[serde(with = "crate::encoding::fixed32")]
    pub viewing_commitment: [u8; 32],
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RoundTwo {
    pub role: Role,
    #[serde(with = "crate::encoding::bytes")]
    pub package: Vec<u8>,
    #[serde(with = "crate::encoding::fixed32")]
    pub viewing_reveal: [u8; 32],
}

pub struct FirstRound {
    role: Role,
    secret: dkg::round1::SecretPackage,
    viewing: Zeroizing<[u8; 32]>,
}

pub struct SecondRound {
    role: Role,
    secret: dkg::round2::SecretPackage,
    peer_first: BTreeMap<frost::Identifier, dkg::round1::Package>,
    peer_commitment: [u8; 32],
    viewing: Zeroizing<[u8; 32]>,
}

#[derive(Clone, Serialize)]
pub struct PublicKeys {
    #[serde(with = "crate::encoding::fixed32")]
    pub group: [u8; 32],
    #[serde(with = "crate::encoding::fixed32")]
    pub payer: [u8; 32],
    #[serde(with = "crate::encoding::fixed32")]
    pub service: [u8; 32],
    #[serde(with = "crate::encoding::bytes")]
    pub viewing_key: Vec<u8>,
    pub address: String,
}

pub struct Keys {
    pub role: Role,
    pub public: PublicKeys,
    pub(crate) package: frost::keys::KeyPackage,
    pub(crate) public_package: frost::keys::PublicKeyPackage,
}

fn commitment(value: &[u8; 32]) -> Result<[u8; 32], KeyError> {
    Ok(encoding::hash(
        b"TidvarFvkCommit",
        &encoding::encode_bytes(&[value])?,
    )?)
}

impl FirstRound {
    pub fn begin<R: RngCore + CryptoRng>(
        role: Role,
        rng: &mut R,
    ) -> Result<(Self, RoundOne), KeyError> {
        let (secret, package) =
            dkg::part1(role.identifier()?, 2, 2, &mut *rng).map_err(|_| KeyError::Message)?;
        let mut viewing = Zeroizing::new([0; 32]);
        rng.fill_bytes(viewing.as_mut());
        let first = RoundOne {
            role,
            package: package.serialize().map_err(|_| KeyError::Message)?,
            viewing_commitment: commitment(&viewing)?,
        };
        Ok((
            Self {
                role,
                secret,
                viewing,
            },
            first,
        ))
    }

    pub fn advance(self, peer: RoundOne) -> Result<(SecondRound, RoundTwo), KeyError> {
        if peer.role != self.role.peer() {
            return Err(KeyError::Message);
        }
        let package =
            dkg::round1::Package::deserialize(&peer.package).map_err(|_| KeyError::Message)?;
        let peer_first = BTreeMap::from([(peer.role.identifier()?, package)]);
        let (secret, mut outgoing) =
            dkg::part2(self.secret, &peer_first).map_err(|_| KeyError::Message)?;
        let package = outgoing
            .remove(&peer.role.identifier()?)
            .ok_or(KeyError::Message)?
            .serialize()
            .map_err(|_| KeyError::Message)?;
        let second = RoundTwo {
            role: self.role,
            package,
            viewing_reveal: *self.viewing,
        };
        Ok((
            SecondRound {
                role: self.role,
                secret,
                peer_first,
                peer_commitment: peer.viewing_commitment,
                viewing: self.viewing,
            },
            second,
        ))
    }
}

fn point(bytes: &[u8; 32]) -> Result<Point, KeyError> {
    Option::<Point>::from(Point::from_bytes(bytes))
        .filter(|p| !bool::from(p.is_identity()))
        .ok_or(KeyError::Group)
}
fn bytes32(bytes: Vec<u8>) -> Result<[u8; 32], KeyError> {
    bytes.try_into().map_err(|_| KeyError::Group)
}

impl SecondRound {
    pub fn finish(self, peer: RoundTwo, network: NetworkType) -> Result<Keys, KeyError> {
        if peer.role != self.role.peer() {
            return Err(KeyError::Message);
        }
        if commitment(&peer.viewing_reveal)? != self.peer_commitment {
            return Err(KeyError::Commitment);
        }
        let received =
            dkg::round2::Package::deserialize(&peer.package).map_err(|_| KeyError::Message)?;
        let (package, public_package) = dkg::part3(
            &self.secret,
            &self.peer_first,
            &BTreeMap::from([(peer.role.identifier()?, received)]),
        )
        .map_err(|_| KeyError::Message)?;
        if !package.has_even_y() || !public_package.has_even_y() {
            return Err(KeyError::Group);
        }
        let group = bytes32(
            public_package
                .verifying_key()
                .serialize()
                .map_err(|_| KeyError::Group)?,
        )?;
        let share = |role: Role| -> Result<[u8; 32], KeyError> {
            bytes32(
                public_package
                    .verifying_shares()
                    .get(&role.identifier()?)
                    .ok_or(KeyError::Group)?
                    .serialize()
                    .map_err(|_| KeyError::Group)?,
            )
        };
        let payer = share(Role::Payer)?;
        let service = share(Role::Service)?;
        if point(&group)?
            != point(&payer)? * pasta_curves::pallas::Scalar::from(2) - point(&service)?
        {
            return Err(KeyError::Group);
        }
        let (payer_random, service_random) = match self.role {
            Role::Payer => (&*self.viewing, &peer.viewing_reveal),
            Role::Service => (&peer.viewing_reveal, &*self.viewing),
        };
        let seed = Zeroizing::new(encoding::hash(
            b"TidvarFvkSeed",
            &encoding::encode_bytes(&[payer_random, service_random])?,
        )?);
        let spending = Option::<orchard::keys::SpendingKey>::from(
            orchard::keys::SpendingKey::from_bytes(*seed),
        )
        .ok_or(KeyError::Group)?;
        let mut viewing = orchard::keys::FullViewingKey::from(&spending).to_bytes();
        viewing[..32].copy_from_slice(&group);
        let fvk = orchard::keys::FullViewingKey::from_bytes(&viewing).ok_or(KeyError::Group)?;
        let receiver = fvk
            .address_at(0u32, orchard::keys::Scope::External)
            .to_raw_address_bytes();
        let address = unified::Address::try_from_items(
            unified::Revision::R0,
            vec![unified::Uitem::Data(unified::Receiver::Orchard(receiver))],
        )
        .map_err(|_| KeyError::Group)?
        .encode(&network);
        Ok(Keys {
            role: self.role,
            public: PublicKeys {
                group,
                payer,
                service,
                viewing_key: viewing.to_vec(),
                address,
            },
            package,
            public_package,
        })
    }
}

impl Keys {
    pub fn restore(
        role: Role,
        secret: &[u8],
        public: &[u8],
        viewing_key: &[u8],
        network: NetworkType,
    ) -> Result<Self, KeyError> {
        if secret.len() > encoding::MAX_MESSAGE_BYTES || public.len() > encoding::MAX_MESSAGE_BYTES
        {
            return Err(KeyError::Message);
        }
        let package =
            frost::keys::KeyPackage::deserialize(secret).map_err(|_| KeyError::Message)?;
        let public_package =
            frost::keys::PublicKeyPackage::deserialize(public).map_err(|_| KeyError::Message)?;
        let derived = frost::keys::VerifyingShare::from(*package.signing_share());
        if package.identifier() != &role.identifier()?
            || *package.min_signers() != 2
            || public_package.min_signers() != Some(2)
            || public_package.verifying_shares().len() != 2
            || derived != *package.verifying_share()
            || public_package.verifying_shares().get(&role.identifier()?) != Some(&derived)
            || public_package.verifying_key() != package.verifying_key()
            || !package.has_even_y()
            || !public_package.has_even_y()
        {
            return Err(KeyError::Group);
        }
        let group = bytes32(
            public_package
                .verifying_key()
                .serialize()
                .map_err(|_| KeyError::Group)?,
        )?;
        let share = |role: Role| -> Result<[u8; 32], KeyError> {
            bytes32(
                public_package
                    .verifying_shares()
                    .get(&role.identifier()?)
                    .ok_or(KeyError::Group)?
                    .serialize()
                    .map_err(|_| KeyError::Group)?,
            )
        };
        let payer = share(Role::Payer)?;
        let service = share(Role::Service)?;
        if point(&group)?
            != point(&payer)? * pasta_curves::pallas::Scalar::from(2) - point(&service)?
        {
            return Err(KeyError::Group);
        }
        let viewing: [u8; 96] = viewing_key.try_into().map_err(|_| KeyError::Group)?;
        if viewing[..32] != group {
            return Err(KeyError::Group);
        }
        let fvk = orchard::keys::FullViewingKey::from_bytes(&viewing).ok_or(KeyError::Group)?;
        let receiver = fvk
            .address_at(0u32, orchard::keys::Scope::External)
            .to_raw_address_bytes();
        let address = unified::Address::try_from_items(
            unified::Revision::R0,
            vec![unified::Uitem::Data(unified::Receiver::Orchard(receiver))],
        )
        .map_err(|_| KeyError::Group)?
        .encode(&network);
        Ok(Self {
            role,
            public: PublicKeys {
                group,
                payer,
                service,
                viewing_key: viewing.to_vec(),
                address,
            },
            package,
            public_package,
        })
    }

    pub fn service_share(&self) -> Result<Zeroizing<[u8; 32]>, KeyError> {
        if self.role != Role::Service {
            return Err(KeyError::Group);
        }
        Ok(Zeroizing::new(bytes32(
            self.package.signing_share().serialize(),
        )?))
    }

    pub fn secret_bytes(&self) -> Result<Zeroizing<Vec<u8>>, KeyError> {
        Ok(Zeroizing::new(
            self.package.serialize().map_err(|_| KeyError::Message)?,
        ))
    }
    pub fn public_bytes(&self) -> Result<Vec<u8>, KeyError> {
        self.public_package
            .serialize()
            .map_err(|_| KeyError::Message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_core::OsRng;
    use zcash_address::unified::Container;

    #[test]
    fn two_independent_parties_derive_the_same_even_key_and_receiver() -> Result<(), KeyError> {
        let (p, p1) = FirstRound::begin(Role::Payer, &mut OsRng)?;
        let (s, s1) = FirstRound::begin(Role::Service, &mut OsRng)?;
        let (p, p2) = p.advance(s1)?;
        let (s, s2) = s.advance(p1)?;
        let p = p.finish(s2, NetworkType::Test)?;
        let s = s.finish(p2, NetworkType::Test)?;
        assert_eq!(p.public.group, s.public.group);
        assert_eq!(p.public.viewing_key, s.public.viewing_key);
        assert_eq!(p.public.address, s.public.address);
        assert!(p.public.address.starts_with("utest1"));
        assert_ne!(*p.secret_bytes()?, *s.secret_bytes()?);
        let restored = Keys::restore(
            Role::Payer,
            &p.secret_bytes()?,
            &p.public_bytes()?,
            &p.public.viewing_key,
            NetworkType::Test,
        )?;
        assert_eq!(restored.public.address, p.public.address);
        assert_eq!(*restored.secret_bytes()?, *p.secret_bytes()?);
        assert!(
            Keys::restore(
                Role::Service,
                &p.secret_bytes()?,
                &p.public_bytes()?,
                &p.public.viewing_key,
                NetworkType::Test
            )
            .is_err()
        );
        assert!(
            Keys::restore(
                Role::Payer,
                &s.secret_bytes()?,
                &p.public_bytes()?,
                &p.public.viewing_key,
                NetworkType::Test
            )
            .is_err()
        );
        let mut changed_viewing = p.public.viewing_key.clone();
        changed_viewing[0] ^= 1;
        assert!(
            Keys::restore(
                Role::Payer,
                &p.secret_bytes()?,
                &p.public_bytes()?,
                &changed_viewing,
                NetworkType::Test
            )
            .is_err()
        );
        let (_, _, ua) =
            unified::Address::decode(&p.public.address).map_err(|_| KeyError::Group)?;
        assert_eq!(ua.items().len(), 1);
        Ok(())
    }

    #[test]
    fn changed_viewing_reveal_is_refused() -> Result<(), KeyError> {
        let (p, p1) = FirstRound::begin(Role::Payer, &mut OsRng)?;
        let (s, s1) = FirstRound::begin(Role::Service, &mut OsRng)?;
        let (p, _) = p.advance(s1)?;
        let (_, mut s2) = s.advance(p1)?;
        s2.viewing_reveal[0] ^= 1;
        assert!(matches!(
            p.finish(s2, NetworkType::Test),
            Err(KeyError::Commitment)
        ));
        Ok(())
    }

    #[test]
    fn own_round_cannot_be_used_as_the_other_party() -> Result<(), KeyError> {
        let (p, p1) = FirstRound::begin(Role::Payer, &mut OsRng)?;
        assert!(p.advance(p1).is_err());
        Ok(())
    }
}
