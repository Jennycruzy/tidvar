use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Deserializer, Serializer};
use thiserror::Error;

pub const MAX_MESSAGE_BYTES: usize = 256 * 1024;

#[derive(Clone, Copy, Debug, Error)]
pub enum EncodingError {
    #[error("The message is too large.")]
    Size,
    #[error("The hash domain is invalid.")]
    Domain,
}

pub fn hash(tag: &[u8], encoded: &[u8]) -> Result<[u8; 32], EncodingError> {
    if !tag.starts_with(b"Tidvar") || tag.len() > 16 {
        return Err(EncodingError::Domain);
    }
    let mut personal = [0; 16];
    personal[..tag.len()].copy_from_slice(tag);
    let value = blake2b_simd::Params::new()
        .hash_length(32)
        .personal(&personal)
        .hash(encoded);
    let mut result = [0; 32];
    result.copy_from_slice(value.as_bytes());
    Ok(result)
}

pub fn encode_bytes(fields: &[&[u8]]) -> Result<Vec<u8>, EncodingError> {
    let mut out = Vec::new();
    for field in fields {
        let length = u32::try_from(field.len()).map_err(|_| EncodingError::Size)?;
        if out.len().saturating_add(field.len()).saturating_add(4) > MAX_MESSAGE_BYTES {
            return Err(EncodingError::Size);
        }
        out.extend_from_slice(&length.to_le_bytes());
        out.extend_from_slice(field);
    }
    Ok(out)
}

pub mod bytes {
    use super::*;
    pub fn serialize<S: Serializer>(value: &[u8], s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&URL_SAFE_NO_PAD.encode(value))
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Vec<u8>, D::Error> {
        let value = String::deserialize(d)?;
        if value.len() > MAX_MESSAGE_BYTES.div_ceil(3) * 4 {
            return Err(serde::de::Error::custom("The message is too large."));
        }
        let decoded = URL_SAFE_NO_PAD
            .decode(&value)
            .map_err(serde::de::Error::custom)?;
        if decoded.len() > MAX_MESSAGE_BYTES {
            return Err(serde::de::Error::custom("The message is too large."));
        }
        Ok(decoded)
    }
}

pub mod fixed32 {
    use super::*;
    pub fn serialize<S: Serializer>(value: &[u8; 32], s: S) -> Result<S::Ok, S::Error> {
        bytes::serialize(value, s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 32], D::Error> {
        bytes::deserialize(d)?
            .try_into()
            .map_err(|_| serde::de::Error::custom("Expected 32 bytes."))
    }
}

pub mod fixed16 {
    use super::*;
    pub fn serialize<S: Serializer>(value: &[u8; 16], s: S) -> Result<S::Ok, S::Error> {
        bytes::serialize(value, s)
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<[u8; 16], D::Error> {
        bytes::deserialize(d)?
            .try_into()
            .map_err(|_| serde::de::Error::custom("Expected 16 bytes."))
    }
}
