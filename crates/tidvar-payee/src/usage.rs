use crate::{PayeeError, store::Store};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use serde::Serialize;

pub const DAILY_BYTES: u64 = 5_000_000_000;
pub const MONTHLY_BYTES: u64 = 6_000_000_000_000;

#[derive(Clone, Copy)]
pub struct Limits {
    pub daily: u64,
    pub monthly: u64,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            daily: DAILY_BYTES,
            monthly: MONTHLY_BYTES,
        }
    }
}
impl Limits {
    pub fn from_environment() -> Result<Self, PayeeError> {
        fn value(name: &str, maximum: u64) -> Result<u64, PayeeError> {
            match std::env::var(name) {
                Ok(value) => value
                    .parse::<u64>()
                    .ok()
                    .filter(|value| *value > 0 && *value <= maximum)
                    .ok_or(PayeeError::Usage),
                Err(std::env::VarError::NotPresent) => Ok(maximum),
                Err(_) => Err(PayeeError::Usage),
            }
        }
        Ok(Self {
            daily: value("TIDVAR_DAILY_BYTES", DAILY_BYTES)?,
            monthly: value("TIDVAR_MONTHLY_BYTES", MONTHLY_BYTES)?,
        })
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub enum Limit {
    #[serde(rename = "DATA_LIMIT")]
    Daily,
    #[serde(rename = "HOST_LIMIT")]
    Monthly,
}
impl Limit {
    pub fn code(self) -> &'static str {
        match self {
            Self::Daily => "DATA_LIMIT",
            Self::Monthly => "HOST_LIMIT",
        }
    }

    pub fn message(self) -> &'static str {
        match self {
            Self::Daily => {
                "You've used today's data allowance. It resets at 00:00 UTC; your prepaid time is kept."
            }
            Self::Monthly => {
                "The VPN has reached this month's data limit for our server. Your prepaid time is kept; close the tab to get it back."
            }
        }
    }
}
#[derive(Clone, Debug, Serialize)]
pub struct Usage {
    pub daily_bytes: u64,
    pub daily_limit: u64,
    pub resets_at_ms: u64,
    pub limit: Option<Limit>,
    #[serde(skip_serializing)]
    pub monthly_bytes: u64,
}
fn period(connection: &Connection, now_ms: u64) -> Result<(u64, String), PayeeError> {
    let month: String = connection
        .query_row(
            "SELECT strftime('%Y-%m',?1,'unixepoch')",
            params![now_ms / 1000],
            |row| row.get(0),
        )
        .map_err(|_| PayeeError::Storage)?;
    Ok((now_ms / 86_400_000, month))
}
fn status(
    connection: &Connection,
    id: &[u8; 32],
    now_ms: u64,
    limits: Limits,
) -> Result<Usage, PayeeError> {
    let (day, month) = period(connection, now_ms)?;
    let daily_bytes: u64 = connection
        .query_row(
            "SELECT bytes FROM daily_traffic WHERE tab_id=?1 AND day=?2",
            params![id.as_slice(), day],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| PayeeError::Storage)?
        .unwrap_or(0);
    let monthly_bytes: u64 = connection
        .query_row(
            "SELECT bytes FROM monthly_traffic WHERE month=?1",
            params![month],
            |row| row.get(0),
        )
        .optional()
        .map_err(|_| PayeeError::Storage)?
        .unwrap_or(0);
    let limit = if monthly_bytes >= limits.monthly {
        Some(Limit::Monthly)
    } else if daily_bytes >= limits.daily {
        Some(Limit::Daily)
    } else {
        None
    };
    let resets_at_ms = if limit == Some(Limit::Monthly) {
        let seconds: String = connection
            .query_row(
                "SELECT strftime('%s',?1,'unixepoch','start of month','+1 month')",
                params![now_ms / 1000],
                |row| row.get(0),
            )
            .map_err(|_| PayeeError::Storage)?;
        seconds
            .parse::<u64>()
            .ok()
            .and_then(|seconds| seconds.checked_mul(1000))
            .ok_or(PayeeError::Usage)?
    } else {
        day.checked_add(1)
            .and_then(|day| day.checked_mul(86_400_000))
            .ok_or(PayeeError::Usage)?
    };
    Ok(Usage {
        daily_bytes,
        daily_limit: limits.daily,
        monthly_bytes,
        limit,
        resets_at_ms,
    })
}
impl Store {
    pub fn usage(&self, id: &[u8; 32], now_ms: u64, limits: Limits) -> Result<Usage, PayeeError> {
        status(&self.connection, id, now_ms, limits)
    }
    pub fn record_transfer(
        &mut self,
        id: &[u8; 32],
        rx: u64,
        tx: u64,
        now_ms: u64,
        limits: Limits,
    ) -> Result<Usage, PayeeError> {
        let transaction = self
            .connection
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|_| PayeeError::Storage)?;
        let previous: Option<(u64, u64, u64)> = transaction
            .query_row(
                "SELECT rx,tx,sampled_ms FROM transfer_counters WHERE tab_id=?1",
                params![id.as_slice()],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .optional()
            .map_err(|_| PayeeError::Storage)?;
        if let Some((old_rx, old_tx, sampled)) = previous {
            if now_ms < sampled {
                return Err(PayeeError::Usage);
            }
            if (old_rx, old_tx) == (rx, tx) {
                let result = status(&transaction, id, now_ms, limits)?;
                transaction.commit().map_err(|_| PayeeError::Storage)?;
                return Ok(result);
            }
        }
        let delta = match previous {
            Some((old_rx, old_tx, _)) => rx
                .checked_sub(old_rx)
                .unwrap_or(rx)
                .checked_add(tx.checked_sub(old_tx).unwrap_or(tx)),
            None => rx.checked_add(tx),
        }
        .ok_or(PayeeError::Usage)?;
        let (day, month) = period(&transaction, now_ms)?;
        let before = status(&transaction, id, now_ms, limits)?;
        let daily = before
            .daily_bytes
            .checked_add(delta)
            .ok_or(PayeeError::Usage)?;
        let monthly = before
            .monthly_bytes
            .checked_add(delta)
            .ok_or(PayeeError::Usage)?;
        transaction.execute(
            "INSERT INTO transfer_counters (tab_id,rx,tx,sampled_ms) VALUES (?1,?2,?3,?4) ON CONFLICT(tab_id) DO UPDATE SET rx=excluded.rx,tx=excluded.tx,sampled_ms=excluded.sampled_ms",
            params![id.as_slice(),rx,tx,now_ms],
        ).map_err(|_|PayeeError::Storage)?;
        transaction.execute(
            "INSERT INTO daily_traffic (tab_id,day,bytes) VALUES (?1,?2,?3) ON CONFLICT(tab_id,day) DO UPDATE SET bytes=excluded.bytes",
            params![id.as_slice(),day,daily],
        ).map_err(|_|PayeeError::Storage)?;
        transaction.execute(
            "INSERT INTO monthly_traffic (month,bytes) VALUES (?1,?2) ON CONFLICT(month) DO UPDATE SET bytes=excluded.bytes",
            params![month,monthly],
        ).map_err(|_|PayeeError::Storage)?;
        let result = status(&transaction, id, now_ms, limits)?;
        transaction.commit().map_err(|_| PayeeError::Storage)?;
        Ok(result)
    }
    pub fn reset_transfer_counter(&mut self, id: &[u8; 32], now_ms: u64) -> Result<(), PayeeError> {
        self.connection.execute(
            "INSERT INTO transfer_counters (tab_id,rx,tx,sampled_ms) VALUES (?1,0,0,?2) ON CONFLICT(tab_id) DO UPDATE SET rx=0,tx=0,sampled_ms=excluded.sampled_ms",
            params![id.as_slice(),now_ms],
        ).map_err(|_|PayeeError::Storage)?;
        Ok(())
    }
}
