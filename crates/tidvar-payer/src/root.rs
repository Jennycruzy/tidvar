use rand_core::{CryptoRng, RngCore};
use sha2::{Digest, Sha256};
use std::sync::OnceLock;
use thiserror::Error;
use tidvar_core::encoding;
use zeroize::Zeroizing;

static WORDS: OnceLock<Result<Vec<String>, RootError>> = OnceLock::new();

#[derive(Clone, Copy, Debug, Error)]
pub enum RootError {
    #[error("The backup phrase must contain 24 valid words with the right checksum.")]
    Phrase,
    #[error("The backup word list could not be read.")]
    Words,
    #[error("The tab key could not be created.")]
    Key,
}

pub struct Root {
    entropy: Zeroizing<[u8; 32]>,
}

fn words() -> Result<&'static [String], RootError> {
    match WORDS.get_or_init(|| {
        let encoded =
            hex::decode(include_str!("english.hex").trim()).map_err(|_| RootError::Words)?;
        if hex::encode(Sha256::digest(&encoded))
            != "2f5eed53a4727b4bf8880d8f3f199efc90e58503646d9ff8eff3a2ed3b24dbda"
        {
            return Err(RootError::Words);
        }
        let text = String::from_utf8(encoded).map_err(|_| RootError::Words)?;
        let words: Vec<_> = text.split_whitespace().map(str::to_owned).collect();
        if words.len() != 2048 {
            return Err(RootError::Words);
        }
        Ok(words)
    }) {
        Ok(words) => Ok(words.as_slice()),
        Err(error) => Err(*error),
    }
}

impl Root {
    pub fn generate<R: RngCore + CryptoRng>(rng: &mut R) -> Self {
        let mut entropy = Zeroizing::new([0; 32]);
        rng.fill_bytes(entropy.as_mut());
        Self { entropy }
    }
    pub fn from_entropy(entropy: [u8; 32]) -> Self {
        Self {
            entropy: Zeroizing::new(entropy),
        }
    }
    pub fn entropy(&self) -> Zeroizing<[u8; 32]> {
        Zeroizing::new(*self.entropy)
    }

    pub fn phrase(&self) -> Result<Zeroizing<String>, RootError> {
        let words = words()?;
        let mut bits = Zeroizing::new([0u8; 33]);
        bits[..32].copy_from_slice(self.entropy.as_ref());
        bits[32] = Sha256::digest(self.entropy.as_ref())[0];
        let mut phrase = Zeroizing::new(String::new());
        for word in 0..24 {
            let mut index = 0usize;
            for bit in 0..11 {
                let position = word * 11 + bit;
                index = (index << 1) | usize::from((bits[position / 8] >> (7 - position % 8)) & 1);
            }
            if word > 0 {
                phrase.push(' ');
            }
            phrase.push_str(words.get(index).ok_or(RootError::Words)?);
        }
        Ok(phrase)
    }

    pub fn from_phrase(phrase: &str) -> Result<Self, RootError> {
        let words = words()?;
        let input: Vec<_> = phrase.split_whitespace().collect();
        if input.len() != 24 {
            return Err(RootError::Phrase);
        }
        let mut bits = Zeroizing::new([0u8; 33]);
        for (word, value) in input.iter().enumerate() {
            let index = words
                .iter()
                .position(|candidate| candidate == value)
                .ok_or(RootError::Phrase)?;
            for bit in 0..11 {
                let position = word * 11 + bit;
                bits[position / 8] |= (((index >> (10 - bit)) & 1) as u8) << (7 - position % 8);
            }
        }
        let mut entropy = Zeroizing::new([0; 32]);
        entropy.copy_from_slice(&bits[..32]);
        if Sha256::digest(entropy.as_ref())[0] != bits[32] {
            return Err(RootError::Phrase);
        }
        Ok(Self { entropy })
    }

    pub fn encryption_key(&self) -> Result<Zeroizing<[u8; 32]>, RootError> {
        let encoded = Zeroizing::new(
            encoding::encode_bytes(&[self.entropy.as_ref()]).map_err(|_| RootError::Key)?,
        );
        Ok(Zeroizing::new(
            encoding::hash(b"TidvarRoot", &encoded).map_err(|_| RootError::Key)?,
        ))
    }

    pub fn signing_key(
        &self,
        tab_id: &[u8; 32],
    ) -> Result<reddsa::SigningKey<reddsa::orchard::SpendAuth>, RootError> {
        let root = self.encryption_key()?;
        for counter in 0..u32::MAX {
            let encoded = Zeroizing::new(
                encoding::encode_bytes(&[root.as_ref(), tab_id, &counter.to_le_bytes()])
                    .map_err(|_| RootError::Key)?,
            );
            let bytes = Zeroizing::new(
                encoding::hash(b"TidvarRecordKey", &encoded).map_err(|_| RootError::Key)?,
            );
            if let Ok(key) = reddsa::SigningKey::try_from(*bytes) {
                return Ok(key);
            }
        }
        Err(RootError::Key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_core::OsRng;
    #[test]
    fn genuine_entropy_round_trips_through_twenty_four_words() -> Result<(), RootError> {
        let root = Root::generate(&mut OsRng);
        let phrase = root.phrase()?;
        assert_eq!(phrase.split_whitespace().count(), 24);
        let restored = Root::from_phrase(&phrase)?;
        assert_eq!(*root.encryption_key()?, *restored.encryption_key()?);
        Ok(())
    }
    #[test]
    fn different_tabs_have_different_record_keys() -> Result<(), RootError> {
        let root = Root::generate(&mut OsRng);
        let mut first = [0; 32];
        let mut second = [0; 32];
        OsRng.fill_bytes(&mut first);
        OsRng.fill_bytes(&mut second);
        let a: [u8; 32] = reddsa::VerificationKey::from(&root.signing_key(&first)?).into();
        let b: [u8; 32] = reddsa::VerificationKey::from(&root.signing_key(&second)?).into();
        assert_ne!(a, b);
        Ok(())
    }
    #[test]
    fn wrong_word_count_is_refused() {
        let root = Root::generate(&mut OsRng);
        let phrase = root.phrase();
        assert!(phrase.is_ok());
        if let Ok(phrase) = phrase {
            let words: Vec<_> = phrase.split_whitespace().take(23).collect();
            assert!(Root::from_phrase(&words.join(" ")).is_err());
        }
    }
}
