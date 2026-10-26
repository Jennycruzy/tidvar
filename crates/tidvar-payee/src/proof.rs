use crate::PayeeError;
use orchard::circuit::{Instance, OrchardCircuitVersion, ProvingKey, VerifyingKey};
use pczt::{
    Pczt,
    roles::{prover::Prover, tx_extractor::TransactionExtractor, verifier::Verifier},
};
use rand_core::{CryptoRng, RngCore};
use tidvar_core::iou::{Checked, Random};

pub struct Proofs {
    proving: ProvingKey,
    verifying: VerifyingKey,
}
impl Proofs {
    pub fn build() -> Self {
        let proving = ProvingKey::build(OrchardCircuitVersion::PostNu6_3);
        let verifying = proving.verifying_key();
        Self { proving, verifying }
    }
    pub fn prove<R: RngCore + CryptoRng>(
        &self,
        checked: Checked,
        rng: &mut R,
    ) -> Result<Pczt, PayeeError> {
        let pczt = Prover::new(checked.pczt)
            .create_ironwood_proof(Random(rng), &self.proving)
            .map_err(|_| PayeeError::Proof)?
            .finish();
        self.verify(pczt)
    }
    pub fn verify(&self, pczt: Pczt) -> Result<Pczt, PayeeError> {
        Verifier::new(pczt)
            .with_ironwood::<(), _>(|bundle| {
                use pczt::roles::verifier::OrchardError;
                let mut instances = Vec::new();
                for action in bundle.actions() {
                    instances.push(
                        Instance::from_parts(
                            *bundle.anchor(),
                            action.cv_net().clone(),
                            *action.spend().nullifier(),
                            action.spend().rk().clone(),
                            *action.output().cmx(),
                            *bundle.flags(),
                        )
                        .ok_or(OrchardError::Custom(()))?,
                    );
                }
                bundle
                    .zkproof()
                    .as_ref()
                    .ok_or(OrchardError::Custom(()))?
                    .verify(&self.verifying, &instances)
                    .map_err(|_| OrchardError::Custom(()))?;
                Ok(())
            })
            .map(|value| value.finish())
            .map_err(|_| PayeeError::Proof)
    }
    pub fn extract<R: RngCore + CryptoRng>(
        &self,
        pczt: Pczt,
        rng: &mut R,
    ) -> Result<zcash_primitives::transaction::Transaction, PayeeError> {
        TransactionExtractor::new(pczt)
            .with_orchard(&self.verifying)
            .extract(Random(rng))
            .map_err(|_| PayeeError::Proof)
    }
}
