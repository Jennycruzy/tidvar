use crate::params::{self, Amount, AmountError};
use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Active {
    at_ms: u64,
    paid_until_ms: u64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ledger {
    paid: Amount,
    vpn_ms: u64,
    active: Option<Active>,
    clock_ms: Option<u64>,
    paused: bool,
    closing: bool,
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct Balance {
    pub paid: Amount,
    pub used: Amount,
    pub vpn_ms: u64,
    pub running: bool,
    pub paused: bool,
    pub closing: bool,
    pub paid_until_ms: Option<u64>,
    pub cut_at_ms: Option<u64>,
}
impl Default for Ledger {
    fn default() -> Self {
        Self {
            paid: Amount::ZERO,
            vpn_ms: 0,
            active: None,
            clock_ms: None,
            paused: false,
            closing: false,
        }
    }
}
impl Ledger {
    pub fn balance(&self) -> Result<Balance, AmountError> {
        let used = params::vpn_used(self.vpn_ms)?.min(self.paid);
        let paid_until_ms = self.active.as_ref().map(|active| active.paid_until_ms);
        let cut_at_ms = paid_until_ms
            .map(|until| {
                until
                    .checked_add(params::CUT_GRACE_MS)
                    .ok_or(AmountError::Time)
            })
            .transpose()?;
        Ok(Balance {
            paid: self.paid,
            used,
            vpn_ms: self.vpn_ms,
            running: self.active.is_some(),
            paused: self.paused,
            closing: self.closing,
            paid_until_ms,
            cut_at_ms,
        })
    }
    pub fn validate(&self, funded: Amount) -> Result<(), AmountError> {
        if self.paid > params::spendable(funded)?
            || self.vpn_ms
                > self
                    .paid
                    .value()
                    .checked_mul(params::VPN_UNIT_MS)
                    .ok_or(AmountError::Time)?
            || (self.active.is_some() && (self.paused || self.closing))
        {
            return Err(AmountError::Used);
        }
        if let Some(active) = &self.active {
            if self.clock_ms != Some(active.at_ms) {
                return Err(AmountError::Time);
            }
            active
                .paid_until_ms
                .checked_add(params::CUT_GRACE_MS)
                .ok_or(AmountError::Time)?;
        }
        Ok(())
    }
    pub fn tick(&mut self, now_ms: u64) -> Result<bool, AmountError> {
        if self.clock_ms.is_some_and(|previous| now_ms < previous) {
            return Err(AmountError::Time);
        }
        let mut removed = false;
        if let Some(active) = &mut self.active {
            let cut = active
                .paid_until_ms
                .checked_add(params::CUT_GRACE_MS)
                .ok_or(AmountError::Time)?;
            let elapsed = now_ms
                .min(cut)
                .checked_sub(active.at_ms)
                .ok_or(AmountError::Time)?;
            self.vpn_ms = self
                .vpn_ms
                .checked_add(elapsed)
                .ok_or(AmountError::Time)?
                .min(
                    self.paid
                        .value()
                        .checked_mul(params::VPN_UNIT_MS)
                        .ok_or(AmountError::Time)?,
                );
            if params::must_remove_peer(now_ms, active.paid_until_ms) {
                self.active = None;
                removed = true;
            } else {
                active.at_ms = now_ms;
            }
        }
        self.clock_ms = Some(now_ms);
        Ok(removed)
    }
    pub fn accept(&mut self, funded: Amount, next: Amount, now_ms: u64) -> Result<(), AmountError> {
        if self.closing {
            return Err(AmountError::Used);
        }
        params::check_payment(funded, self.paid, next)?;
        self.tick(now_ms)?;
        self.paid = next;
        let used = self.balance()?.used;
        if let Some(active) = &mut self.active {
            active.paid_until_ms = params::paid_until_ms(now_ms, next, used)?;
        }
        Ok(())
    }
    pub fn resume(&mut self, now_ms: u64) -> Result<(), AmountError> {
        self.tick(now_ms)?;
        if self.closing || !params::can_start_vpn(self.paid, self.balance()?.used) {
            return Err(AmountError::Used);
        }
        if self.active.is_none() {
            self.active = Some(Active {
                at_ms: now_ms,
                paid_until_ms: params::paid_until_ms(now_ms, self.paid, self.balance()?.used)?,
            });
        }
        self.paused = false;
        Ok(())
    }
    pub fn pause(&mut self, now_ms: u64) -> Result<(), AmountError> {
        self.tick(now_ms)?;
        self.active = None;
        self.paused = true;
        Ok(())
    }
    pub fn close(&mut self, now_ms: u64) -> Result<Balance, AmountError> {
        self.pause(now_ms)?;
        self.closing = true;
        self.balance()
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    fn now() -> Result<u64, Box<dyn std::error::Error>> {
        Ok(u64::try_from(
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)?
                .as_millis(),
        )?)
    }
    #[test]
    fn clock_updates_never_extend_the_saved_cutoff() -> Result<(), Box<dyn std::error::Error>> {
        let start = now()?;
        let funded = Amount::try_from(100_000)?;
        let mut ledger = Ledger::default();
        assert!(ledger.resume(start).is_err());
        ledger.accept(funded, Amount::try_from(4)?, start)?;
        ledger.resume(start)?;
        let cut = ledger.balance()?.cut_at_ms.ok_or("Missing cutoff")?;
        assert_eq!(cut - start, 29_000);
        for elapsed in [1, 1000, 5999, 6000, 10_000, 24_000, 29_000] {
            assert!(!ledger.tick(start + elapsed)?);
            assert_eq!(ledger.balance()?.cut_at_ms, Some(cut));
        }
        assert!(ledger.tick(cut + 1)?);
        assert!(!ledger.balance()?.running);
        assert_eq!(ledger.balance()?.used.value(), 4);
        ledger.validate(funded)?;
        Ok(())
    }
    #[test]
    fn pause_and_recovery_preserve_partial_seconds_and_credit()
    -> Result<(), Box<dyn std::error::Error>> {
        let start = now()?;
        let funded = Amount::try_from(100_000)?;
        let mut ledger = Ledger::default();
        ledger.accept(funded, Amount::try_from(600)?, start)?;
        ledger.resume(start)?;
        ledger.pause(start + 1250)?;
        let bytes = serde_json::to_vec(&ledger)?;
        let mut ledger: Ledger = serde_json::from_slice(&bytes)?;
        ledger.tick(start + 900_000)?;
        assert_eq!(ledger.balance()?.vpn_ms, 1250);
        assert_eq!(ledger.balance()?.used.value(), 1);
        ledger.resume(start + 900_000)?;
        ledger.pause(start + 901_250)?;
        assert_eq!(ledger.balance()?.vpn_ms, 2500);
        assert_eq!(ledger.balance()?.used.value(), 1);
        let close = ledger.close(start + 901_250)?;
        assert!(close.used <= close.paid && close.closing);
        assert!(ledger.resume(start + 901_251).is_err());
        assert!(
            ledger
                .accept(funded, Amount::try_from(601)?, start + 901_251)
                .is_err()
        );
        Ok(())
    }
    #[test]
    fn accepted_payments_extend_time_once_and_refuse_replays()
    -> Result<(), Box<dyn std::error::Error>> {
        let start = now()?;
        let funded = Amount::try_from(100_000)?;
        let mut ledger = Ledger::default();
        ledger.accept(funded, Amount::try_from(4)?, start)?;
        ledger.resume(start)?;
        ledger.accept(funded, Amount::try_from(6)?, start + 10_000)?;
        let balance = ledger.balance()?;
        assert_eq!(balance.used.value(), 2);
        assert_eq!(balance.cut_at_ms, Some(start + 39_000));
        assert!(
            ledger
                .accept(funded, Amount::try_from(6)?, start + 11_000)
                .is_err()
        );
        assert!(ledger.tick(start + 9999).is_err());
        assert_eq!(ledger.balance()?, balance);
        let mut restored: Ledger = serde_json::from_slice(&serde_json::to_vec(&ledger)?)?;
        restored.tick(start + 39_001)?;
        assert!(!restored.balance()?.running);
        restored.validate(funded)?;
        Ok(())
    }
}
