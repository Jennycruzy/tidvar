//! Attack suite, local part (SPEC 12.2): each attack must be refused with real cryptography.
use ff::PrimeField;
use pasta_curves::pallas::Scalar;
use rand_core::{OsRng, RngCore};
use tidvar_core::{
    encoding,
    frost::{Message, Nonce, Share, aggregate, verify_share},
    keys::{FirstRound, Keys, Role},
    transport::{Authenticated, Id},
};
use zcash_protocol::consensus::NetworkType;

type Result<T> = std::result::Result<T, Box<dyn std::error::Error>>;

fn pair() -> Result<(Keys, Keys)> {
    let (p, p1) = FirstRound::begin(Role::Payer, &mut OsRng)?;
    let (s, s1) = FirstRound::begin(Role::Service, &mut OsRng)?;
    let (p, p2) = p.advance(s1)?;
    let (s, s2) = s.advance(p1)?;
    Ok((
        p.finish(s2, NetworkType::Test)?,
        s.finish(p2, NetworkType::Test)?,
    ))
}
fn scalar() -> [u8; 32] {
    loop {
        let mut bytes = [0; 32];
        OsRng.fill_bytes(&mut bytes);
        if Option::<Scalar>::from(Scalar::from_repr(bytes)).is_some() {
            return bytes;
        }
    }
}

/// AT2: the service tries to close for more than it was paid by forging the payer's share.
#[test]
fn at2_forged_payer_share_cannot_be_aggregated() -> Result<()> {
    let (p, s) = pair()?;
    let pn = Nonce::generate(&p, &mut OsRng)?;
    let sn = Nonce::generate(&s, &mut OsRng)?;
    let message = Message {
        sighash: encoding::hash(b"TidvarAttack2", &p.public_bytes()?)?,
        alpha: scalar(),
        payer: pn.public().clone(),
        service: sn.public().clone(),
    };
    let honest = pn.sign(&p, &message)?;
    let service = sn.sign(&s, &message)?;
    let forged = Share {
        role: Role::Payer,
        value: scalar(),
    };
    assert!(verify_share(&s, &message, &forged).is_err());
    assert!(aggregate(&s, &message, &forged, &service).is_err());
    // The honest payer share over a different (larger) payment is refused too.
    let mut larger = message.clone();
    larger.sighash[0] ^= 1;
    assert!(aggregate(&s, &larger, &honest, &service).is_err());
    assert!(aggregate(&s, &message, &honest, &service).is_ok());
    Ok(())
}

/// AT7: a rogue key in the DKG (round-one package without a valid proof of knowledge).
#[test]
fn at7_rogue_dkg_package_aborts_the_setup() -> Result<()> {
    let (s, _) = FirstRound::begin(Role::Service, &mut OsRng)?;
    let (_, mut rogue) = FirstRound::begin(Role::Payer, &mut OsRng)?;
    let last = rogue.package.len() - 1;
    rogue.package[last] ^= 1;
    assert!(s.advance(rogue).is_err());
    let (s, _) = FirstRound::begin(Role::Service, &mut OsRng)?;
    let (_, mut same_role) = FirstRound::begin(Role::Service, &mut OsRng)?;
    same_role.role = Role::Service;
    assert!(s.advance(same_role).is_err());
    Ok(())
}

/// AT13: a request signed for tab A is presented for tab B.
#[test]
fn at13_message_for_one_tab_is_refused_for_another() -> Result<()> {
    let key_a = reddsa::SigningKey::<reddsa::orchard::SpendAuth>::new(OsRng);
    let key_b = reddsa::SigningKey::<reddsa::orchard::SpendAuth>::new(OsRng);
    let message = tidvar_core::transport::Message {
        tab_id: Id([1; 32]),
        k: 1,
        previous: None,
        body: serde_json::json!({"step":"vpn_pause"}),
    };
    let signed = Authenticated::sign(message, &key_a, &mut OsRng)?;
    assert!(
        signed
            .verify(reddsa::VerificationKey::from(&key_a).into())
            .is_ok()
    );
    assert!(
        signed
            .verify(reddsa::VerificationKey::from(&key_b).into())
            .is_err()
    );
    let mut moved = signed.clone();
    moved.message.tab_id = Id([2; 32]);
    assert!(
        moved
            .verify(reddsa::VerificationKey::from(&key_a).into())
            .is_err()
    );
    assert!(!signed.message.follows(Id([2; 32]), 1, None));
    Ok(())
}
