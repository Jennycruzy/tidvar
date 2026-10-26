use crate::encoding::{self, EncodingError};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Eq, PartialEq, Serialize, Deserialize)]
pub struct Id(#[serde(with = "crate::encoding::fixed32")] pub [u8; 32]);
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Message<T> {
    pub tab_id: Id,
    pub k: u64,
    pub previous: Option<Id>,
    pub body: T,
}
fn append(out: &mut Vec<u8>, value: &Value) -> Result<(), EncodingError> {
    match value {
        Value::Null => out.push(0),
        Value::Bool(v) => out.extend([1, u8::from(*v)]),
        Value::Number(v) => {
            if let Some(v) = v.as_u64() {
                out.push(2);
                out.extend(v.to_le_bytes());
            } else if let Some(v) = v.as_i64() {
                out.push(3);
                out.extend(v.to_le_bytes());
            } else {
                return Err(EncodingError::Domain);
            }
        }
        Value::String(v) => {
            out.push(4);
            out.extend(encoding::encode_bytes(&[v.as_bytes()])?);
        }
        Value::Array(values) => {
            out.push(5);
            out.extend(
                u32::try_from(values.len())
                    .map_err(|_| EncodingError::Size)?
                    .to_le_bytes(),
            );
            for value in values {
                append(out, value)?;
            }
        }
        Value::Object(values) => {
            out.push(6);
            out.extend(
                u32::try_from(values.len())
                    .map_err(|_| EncodingError::Size)?
                    .to_le_bytes(),
            );
            let mut keys: Vec<_> = values.keys().collect();
            keys.sort();
            for key in keys {
                out.extend(encoding::encode_bytes(&[key.as_bytes()])?);
                append(out, &values[key])?;
            }
        }
    }
    if out.len() > encoding::MAX_MESSAGE_BYTES {
        return Err(EncodingError::Size);
    }
    Ok(())
}
impl<T: Serialize> Message<T> {
    pub fn hash(&self) -> Result<Id, EncodingError> {
        let mut encoded = self.tab_id.0.to_vec();
        encoded.extend(self.k.to_le_bytes());
        match self.previous {
            Some(id) => {
                encoded.push(1);
                encoded.extend(id.0);
            }
            None => encoded.push(0),
        }
        let value = serde_json::to_value(&self.body).map_err(|_| EncodingError::Domain)?;
        append(&mut encoded, &value)?;
        Ok(Id(encoding::hash(b"TidvarMessage", &encoded)?))
    }
    pub fn follows(&self, id: Id, k: u64, previous: Option<Id>) -> bool {
        self.tab_id == id && self.k == k && self.previous == previous && self.k > 0
    }
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Authenticated {
    pub message: Message<Value>,
    #[serde(with = "crate::encoding::bytes")]
    pub signature: Vec<u8>,
}
impl Authenticated {
    pub fn sign<R: rand_core::RngCore + rand_core::CryptoRng>(
        message: Message<Value>,
        key: &reddsa::SigningKey<reddsa::orchard::SpendAuth>,
        rng: &mut R,
    ) -> Result<Self, EncodingError> {
        let signature: [u8; 64] = key.sign(rng, &message.hash()?.0).into();
        Ok(Self {
            message,
            signature: signature.to_vec(),
        })
    }
    pub fn verify(&self, public: [u8; 32]) -> Result<(), EncodingError> {
        let signature: [u8; 64] = self
            .signature
            .as_slice()
            .try_into()
            .map_err(|_| EncodingError::Domain)?;
        reddsa::VerificationKey::<reddsa::orchard::SpendAuth>::try_from(public)
            .map_err(|_| EncodingError::Domain)?
            .verify(&self.message.hash()?.0, &reddsa::Signature::from(signature))
            .map_err(|_| EncodingError::Domain)
    }
}
