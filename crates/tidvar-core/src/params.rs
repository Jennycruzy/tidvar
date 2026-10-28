use serde::{Deserialize, Serialize};
use thiserror::Error;

pub const MINIMUM_FUNDING: u64 = 50_000;
pub const FEE_RESERVE: u64 = 20_000;
pub const MAXIMUM_NOTES: u8 = 4;
pub const MAXIMUM_EXPIRY: u32 = 499_999_999;
pub const MAXIMUM_MONEY: u64 = 2_100_000_000_000_000;
pub const IOU_INTERVAL_MS: u64 = 10_000;
pub const VISIBLE_ADVANCE_MS: u64 = 20_000;
pub const AWAY_ADVANCE_MS: u64 = 3_600_000;
pub const CUT_GRACE_MS: u64 = 5_000;
/// One chat answer: 1,000 zatoshi (about one cent).
pub const CHAT_PRICE: u64 = 1_000;
pub const VPN_UNIT_MS: u64 = 6_000;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Error)]
pub enum AmountError {
    #[error("The amount is outside the Zcash money range.")]
    Range,
    #[error("Open a tab with at least 50000 zat.")]
    Minimum,
    #[error("A tab must spend between one and four funding notes.")]
    Notes,
    #[error("The next payment must increase and fit within the tab.")]
    Payment,
    #[error("The closing payment cannot exceed the amount signed.")]
    Used,
    #[error("The lifetime must be positive and fit within the supported clock range.")]
    Time,
    #[error("The transaction expiry is outside the supported height range.")]
    Expiry,
}

#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct Amount(u64);

impl TryFrom<u64> for Amount {
    type Error = AmountError;

    fn try_from(value: u64) -> Result<Self, Self::Error> {
        if value > MAXIMUM_MONEY {
            return Err(AmountError::Range);
        }
        Ok(Self(value))
    }
}

impl From<Amount> for u64 {
    fn from(value: Amount) -> Self {
        value.0
    }
}

impl Amount {
    pub const ZERO: Self = Self(0);

    pub const fn value(self) -> u64 {
        self.0
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Settlement {
    pub service: Amount,
    pub payer: Amount,
    pub fee: Amount,
}

/// The smallest top-up a tab takes (0.0001), so a note never costs more in fees than it adds.
pub const MINIMUM_TOP_UP: u64 = 10_000;

pub fn close_fee(note_count: u8) -> Result<Amount, AmountError> {
    if !(1..=MAXIMUM_NOTES).contains(&note_count) {
        return Err(AmountError::Notes);
    }
    Amount::try_from(5_000 * u64::from(note_count.max(2)))
}

pub fn spendable(funded: Amount) -> Result<Amount, AmountError> {
    if funded.0 < MINIMUM_FUNDING {
        return Err(AmountError::Minimum);
    }
    Amount::try_from(funded.0 - FEE_RESERVE)
}

pub fn check_payment(funded: Amount, previous: Amount, next: Amount) -> Result<(), AmountError> {
    if next <= previous || next > spendable(funded)? {
        return Err(AmountError::Payment);
    }
    Ok(())
}

pub fn settlement(
    funded: Amount,
    notes: u8,
    signed: Amount,
    used: Amount,
) -> Result<Settlement, AmountError> {
    let maximum = spendable(funded)?;
    if used > signed || signed > maximum {
        return Err(AmountError::Used);
    }
    let fee = close_fee(notes)?;
    let payer = funded
        .0
        .checked_sub(used.0)
        .and_then(|v| v.checked_sub(fee.0))
        .ok_or(AmountError::Range)?;
    Ok(Settlement {
        service: used,
        payer: Amount::try_from(payer)?,
        fee,
    })
}

pub fn vpn_used(elapsed_ms: u64) -> Result<Amount, AmountError> {
    Amount::try_from(elapsed_ms.div_ceil(VPN_UNIT_MS))
}

pub fn payment_target(funded: Amount, used: Amount, hidden: bool) -> Result<Amount, AmountError> {
    let maximum = spendable(funded)?;
    if used > maximum {
        return Err(AmountError::Used);
    }
    let advance = if hidden {
        AWAY_ADVANCE_MS
    } else {
        VISIBLE_ADVANCE_MS
    };
    Amount::try_from(
        used.0
            .saturating_add(advance.div_ceil(VPN_UNIT_MS))
            .min(maximum.0),
    )
}

pub fn can_start_vpn(paid: Amount, used: Amount) -> bool {
    used.0
        .checked_add(VISIBLE_ADVANCE_MS.div_ceil(VPN_UNIT_MS))
        .is_some_and(|minimum| paid.0 >= minimum)
}

pub fn paid_until_ms(now_ms: u64, paid: Amount, used: Amount) -> Result<u64, AmountError> {
    let credit = paid.0.checked_sub(used.0).ok_or(AmountError::Used)?;
    now_ms
        .checked_add(credit.checked_mul(VPN_UNIT_MS).ok_or(AmountError::Time)?)
        .ok_or(AmountError::Time)
}

pub fn must_remove_peer(now_ms: u64, paid_until_ms: u64) -> bool {
    now_ms.saturating_sub(paid_until_ms) > CUT_GRACE_MS && now_ms > paid_until_ms
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Lifetime {
    /// Testnet only: lets the refund and the forced close be shown end to end in minutes.
    Minutes15,
    /// A short session: the forced close comes after 1 hour and the refund after 2.
    Hours2,
    Days3,
    Days7,
    Days30,
}

impl Lifetime {
    pub const fn seconds(self) -> u64 {
        match self {
            Self::Minutes15 => 15 * 60,
            Self::Hours2 => 2 * 3_600,
            Self::Days3 => 3 * 86_400,
            Self::Days7 => 7 * 86_400,
            Self::Days30 => 30 * 86_400,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Deadlines {
    pub pack_ms: u64,
    pub watch_ms: u64,
    pub close_ms: u64,
    pub refund_ms: u64,
}

impl Deadlines {
    pub fn new(open_ms: u64, lifetime_ms: u64) -> Result<Self, AmountError> {
        if lifetime_ms < 8 {
            return Err(AmountError::Time);
        }
        let refund = open_ms.checked_add(lifetime_ms).ok_or(AmountError::Time)?;
        let fraction = |numerator: u64, denominator: u64| -> Result<u64, AmountError> {
            lifetime_ms
                .checked_mul(numerator)
                .map(|v| v / denominator)
                .ok_or(AmountError::Time)
        };
        let result = Self {
            pack_ms: refund - (18 * 3_600_000).min(fraction(7, 8)?),
            watch_ms: refund - (12 * 3_600_000).min(fraction(3, 4)?),
            close_ms: refund - (6 * 3_600_000).min(lifetime_ms / 2),
            refund_ms: refund,
        };
        if !(open_ms < result.pack_ms
            && result.pack_ms < result.watch_ms
            && result.watch_ms < result.close_ms
            && result.close_ms < result.refund_ms)
        {
            return Err(AmountError::Time);
        }
        Ok(result)
    }
}

pub fn iou_expiry(open_height: u32, lifetime_ms: u64) -> Result<u32, AmountError> {
    if lifetime_ms == 0 {
        return Err(AmountError::Time);
    }
    let blocks = lifetime_ms.div_ceil(20_000);
    let expiry = u64::from(open_height)
        .checked_add(blocks)
        .and_then(|v| v.checked_add(40))
        .ok_or(AmountError::Expiry)?;
    let expiry = u32::try_from(expiry).map_err(|_| AmountError::Expiry)?;
    if expiry > MAXIMUM_EXPIRY {
        return Err(AmountError::Expiry);
    }
    Ok(expiry)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fees_and_unused_reserve_return_to_payer() -> Result<(), AmountError> {
        let funded = Amount::try_from(100_000)?;
        let maximum = spendable(funded)?;
        for (notes, fee) in [(1, 10_000), (2, 10_000), (3, 15_000), (4, 20_000)] {
            let closed = settlement(funded, notes, maximum, maximum)?;
            assert_eq!(closed.fee.value(), fee);
            assert_eq!(closed.payer.value(), FEE_RESERVE - fee);
            assert_eq!(
                closed.service.value() + closed.payer.value() + closed.fee.value(),
                funded.value()
            );
        }
        assert!(close_fee(0).is_err());
        assert!(close_fee(5).is_err());
        Ok(())
    }

    #[test]
    fn payment_rounding_and_admission_follow_actual_rates() -> Result<(), AmountError> {
        let zero = Amount::try_from(0)?;
        let funded = Amount::try_from(100_000)?;
        assert_eq!(vpn_used(0)?.value(), 0);
        assert_eq!(vpn_used(1)?.value(), 1);
        assert_eq!(vpn_used(6_000)?.value(), 1);
        assert_eq!(vpn_used(6_001)?.value(), 2);
        assert_eq!(payment_target(funded, zero, false)?.value(), 4);
        assert_eq!(payment_target(funded, zero, true)?.value(), 600);
        assert!(can_start_vpn(Amount::try_from(4)?, zero));
        assert!(!can_start_vpn(Amount::try_from(3)?, zero));
        let deadline = paid_until_ms(10_000, Amount::try_from(4)?, zero)?;
        assert!(!must_remove_peer(deadline + 5_000, deadline));
        assert!(must_remove_peer(deadline + 5_001, deadline));
        Ok(())
    }

    #[test]
    fn payment_and_close_cannot_exceed_signed_budget() -> Result<(), AmountError> {
        let funded = Amount::try_from(100_000)?;
        let zero = Amount::try_from(0)?;
        let paid = Amount::try_from(600)?;
        check_payment(funded, zero, paid)?;
        assert!(check_payment(funded, paid, paid).is_err());
        assert!(check_payment(funded, paid, Amount::try_from(80_001)?).is_err());
        assert!(settlement(funded, 1, paid, Amount::try_from(601)?).is_err());
        assert_eq!(
            settlement(funded, 1, paid, Amount::try_from(1)?)?
                .payer
                .value(),
            89_999
        );
        Ok(())
    }

    #[test]
    fn safety_windows_keep_their_order_for_short_and_long_tabs() -> Result<(), AmountError> {
        let short = Deadlines::new(0, 900_000)?;
        assert_eq!(
            (
                short.pack_ms,
                short.watch_ms,
                short.close_ms,
                short.refund_ms
            ),
            (112_500, 225_000, 450_000, 900_000)
        );
        let session = Deadlines::new(0, Lifetime::Hours2.seconds() * 1000)?;
        assert_eq!(
            (
                session.pack_ms,
                session.watch_ms,
                session.close_ms,
                session.refund_ms
            ),
            (900_000, 1_800_000, 3_600_000, 7_200_000)
        );
        for duration in [Lifetime::Days3, Lifetime::Days7, Lifetime::Days30] {
            let seconds = duration.seconds();
            let d = Deadlines::new(0, seconds * 1000)?;
            assert_eq!(d.refund_ms - d.pack_ms, 18 * 3_600_000);
            assert_eq!(d.refund_ms - d.watch_ms, 12 * 3_600_000);
            assert_eq!(d.refund_ms - d.close_ms, 6 * 3_600_000);
        }
        assert_eq!(
            iou_expiry(4_536_866, Lifetime::Days3.seconds() * 1000)?,
            4_549_866
        );
        assert!(Deadlines::new(u64::MAX, 900_000).is_err());
        assert!(iou_expiry(MAXIMUM_EXPIRY, 1).is_err());
        Ok(())
    }
}
