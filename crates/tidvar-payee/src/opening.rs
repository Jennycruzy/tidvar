use crate::PayeeError;
use rand_core::{CryptoRng, RngCore};
use tidvar_core::{
    keys::{FirstRound, Keys, Role, RoundOne, RoundTwo, SecondRound},
    record::{Offer, RecordBody, ServiceSigned, SignedRecord, Terms},
};
use tidvar_seal::{
    quicknet_public_key, round_at,
    seal::{Challenge, Opening, SealedKey, SecretSeal},
};

pub struct Setup {
    second: SecondRound,
    offer: Offer,
    terms: Terms,
}
pub struct Sealed {
    keys: Keys,
    secret: SecretSeal,
    offer: Offer,
    terms: Terms,
    challenge: Option<Challenge>,
    record_hash: Option<[u8; 32]>,
}
pub struct Accepted {
    pub keys: Keys,
    pub record: SignedRecord,
    pub sealed: SealedKey,
}
impl Setup {
    pub fn begin<R: RngCore + CryptoRng>(
        offer: Offer,
        terms: Terms,
        peer: RoundOne,
        rng: &mut R,
    ) -> Result<(Self, RoundOne, RoundTwo), PayeeError> {
        terms.validate(&offer)?;
        let (first, round_one) = FirstRound::begin(Role::Service, rng)?;
        let (second, round_two) = first.advance(peer)?;
        Ok((
            Self {
                second,
                offer,
                terms,
            },
            round_one,
            round_two,
        ))
    }
    pub fn finish<R: RngCore + CryptoRng>(
        self,
        peer: RoundTwo,
        rng: &mut R,
    ) -> Result<Sealed, PayeeError> {
        let keys = self.second.finish(peer, self.offer.network.kind())?;
        let deadline = self.terms.validate(&self.offer)?;
        let round = round_at(deadline.refund_ms / 1000)?;
        let secret =
            SecretSeal::create(&*keys.service_share()?, &quicknet_public_key()?, round, rng)?;
        Ok(Sealed {
            keys,
            secret,
            offer: self.offer,
            terms: self.terms,
            challenge: None,
            record_hash: None,
        })
    }
}
impl Sealed {
    pub fn sealed(&self) -> &SealedKey {
        &self.secret.sealed
    }
    pub fn answer(&mut self, challenge: Challenge) -> Result<Vec<Opening>, PayeeError> {
        let result = self.secret.answer(&challenge)?;
        self.challenge = Some(challenge);
        Ok(result)
    }
    fn expected(&self, payer_key: [u8; 32]) -> Result<RecordBody, PayeeError> {
        let challenge = self.challenge.as_ref().ok_or(PayeeError::Terms)?;
        Ok(RecordBody::new(
            self.offer.clone(),
            self.terms.clone(),
            &self.keys.public,
            self.secret.sealed.commitment()?,
            challenge.indices.clone(),
            payer_key,
        )?)
    }
    pub fn sign<R: RngCore + CryptoRng>(
        &mut self,
        body: RecordBody,
        key: &reddsa::SigningKey<reddsa::orchard::SpendAuth>,
        rng: &mut R,
    ) -> Result<ServiceSigned, PayeeError> {
        let expected = self.expected(body.payer_record_key)?;
        if expected.canonical()? != body.canonical()? {
            return Err(PayeeError::Terms);
        }
        let hash = body.hash()?;
        if self.record_hash.is_some_and(|previous| previous != hash) {
            return Err(PayeeError::Terms);
        }
        let signed = ServiceSigned::sign(body, key, rng)?;
        self.record_hash = Some(hash);
        Ok(signed)
    }
    pub fn accept(self, record: SignedRecord) -> Result<Accepted, PayeeError> {
        record.verify()?;
        if self.record_hash != Some(record.body.hash()?)
            || self.expected(record.body.payer_record_key)?.canonical()?
                != record.body.canonical()?
        {
            return Err(PayeeError::Terms);
        }
        Ok(Accepted {
            keys: self.keys,
            record,
            sealed: self.secret.sealed.clone(),
        })
    }
}
impl Accepted {
    pub fn verify(&self) -> Result<(), PayeeError> {
        self.keys.service_share()?;
        self.record.verify()?;
        let body = &self.record.body;
        if self.sealed.commitment()? != body.seal_commitment
            || self.sealed.round != body.refund_round
            || self
                .sealed
                .coefficients
                .first()
                .is_none_or(|c| c.0 != self.keys.public.service)
        {
            return Err(PayeeError::Terms);
        }
        let expected = RecordBody::new(
            body.offer.clone(),
            body.terms.clone(),
            &self.keys.public,
            body.seal_commitment,
            body.challenge.clone(),
            body.payer_record_key,
        )?;
        if expected.canonical()? != body.canonical()? {
            return Err(PayeeError::Terms);
        }
        Ok(())
    }
}
