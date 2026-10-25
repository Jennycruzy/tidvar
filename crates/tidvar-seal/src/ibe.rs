// Adapted from the tlock 0.0.10 IBE implementation, distributed under the MIT License.
// Copyright (c) 2023 Thibault Meunier; portions Copyright (c) 2022 Timofey Luin.
// The full upstream notice is in THIRD_PARTY_NOTICES.md.
use ark_bls12_381::{Bls12_381, Fr, G1Affine, G1Projective, G2Affine};
use ark_ec::{
    AffineRepr, CurveGroup,
    hashing::{HashToCurve, curve_maps::wb::WBMap, map_to_curve_hasher::MapToCurveBasedHasher},
    pairing::{Pairing, PairingOutput},
};
use ark_ff::field_hashers::DefaultFieldHasher;
use ark_serialize::{CanonicalDeserialize, CanonicalSerialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

const DOMAIN: &[u8] = b"BLS_SIG_BLS12381G1_XMD:SHA-256_SSWU_RO_NUL_";

#[derive(Clone, Copy, Debug, Error)]
#[error("The time-sealed ciphertext is invalid.")]
pub struct IbeError;

fn exponent(key: &[u8; 16], sigma: &[u8; 16]) -> Result<Fr, IbeError> {
    let input = Sha256::new()
        .chain_update(b"IBE-H3")
        .chain_update(sigma)
        .chain_update(key)
        .finalize();
    for counter in 1..u16::MAX {
        let mut candidate = Sha256::new()
            .chain_update(counter.to_le_bytes())
            .chain_update(input)
            .finalize();
        candidate[0] >>= 1;
        candidate.reverse();
        if let Ok(value) = Fr::deserialize_compressed(&candidate[..]) {
            return Ok(value);
        }
    }
    Err(IbeError)
}

fn identity(round: u64) -> Result<G1Affine, IbeError> {
    if round == 0 {
        return Err(IbeError);
    }
    let mapper = MapToCurveBasedHasher::<
        G1Projective,
        DefaultFieldHasher<Sha256, 128>,
        WBMap<<G1Projective as CurveGroup>::Config>,
    >::new(DOMAIN)
    .map_err(|_| IbeError)?;
    mapper
        .hash(&Sha256::digest(round.to_be_bytes()))
        .map_err(|_| IbeError)
}

fn pairing_mask(value: PairingOutput<Bls12_381>) -> Result<[u8; 32], IbeError> {
    let mut encoded = Vec::new();
    value
        .serialize_compressed(&mut encoded)
        .map_err(|_| IbeError)?;
    encoded.reverse();
    Ok(Sha256::new()
        .chain_update(b"IBE-H2")
        .chain_update(encoded)
        .finalize()
        .into())
}

pub fn encrypt_with_sigma(
    public: &[u8],
    round: u64,
    key: &[u8; 16],
    sigma: &[u8; 16],
) -> Result<Vec<u8>, IbeError> {
    if public.len() != 96 {
        return Err(IbeError);
    }
    let public = G2Affine::deserialize_compressed(public).map_err(|_| IbeError)?;
    if public.is_zero() {
        return Err(IbeError);
    }
    let power = exponent(key, sigma)?;
    let u = (G2Affine::generator() * power).into_affine();
    let mask = pairing_mask(Bls12_381::pairing(identity(round)?, public) * power)?;
    let mut ciphertext = Vec::with_capacity(128);
    u.serialize_compressed(&mut ciphertext)
        .map_err(|_| IbeError)?;
    ciphertext.extend((0..16).map(|i| sigma[i] ^ mask[i]));
    let mask = Sha256::new()
        .chain_update(b"IBE-H4")
        .chain_update(sigma)
        .finalize();
    ciphertext.extend((0..16).map(|i| key[i] ^ mask[i]));
    Ok(ciphertext)
}

pub fn decrypt_exact(
    signature: &[u8],
    ciphertext: &[u8],
) -> Result<([u8; 16], [u8; 16]), IbeError> {
    if signature.len() != 48 || ciphertext.len() != 128 {
        return Err(IbeError);
    }
    let signature = G1Affine::deserialize_compressed(signature).map_err(|_| IbeError)?;
    let u = G2Affine::deserialize_compressed(&ciphertext[..96]).map_err(|_| IbeError)?;
    if signature.is_zero() || u.is_zero() {
        return Err(IbeError);
    }
    let mask = pairing_mask(Bls12_381::pairing(signature, u))?;
    let mut sigma = [0; 16];
    for (i, value) in sigma.iter_mut().enumerate() {
        *value = ciphertext[96 + i] ^ mask[i];
    }
    let mask = Sha256::new()
        .chain_update(b"IBE-H4")
        .chain_update(sigma)
        .finalize();
    let mut key = [0; 16];
    for (i, value) in key.iter_mut().enumerate() {
        *value = ciphertext[112 + i] ^ mask[i];
    }
    if (G2Affine::generator() * exponent(&key, &sigma)?).into_affine() != u {
        return Err(IbeError);
    }
    Ok((key, sigma))
}
