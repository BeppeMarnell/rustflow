use std::time::Duration;

use sqlx::{PgPool, Row};
use thiserror::Error;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RetentionSettings {
    pub sweep_interval: Duration,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum RetentionConfigError {
    #[error("RETENTION_SWEEP_SECONDS must be an integer from 60 to 86400")]
    InvalidInterval,
}

impl RetentionSettings {
    pub fn from_env() -> Result<Self, RetentionConfigError> {
        Self::parse(std::env::var("RETENTION_SWEEP_SECONDS").ok().as_deref())
    }

    pub fn parse(raw: Option<&str>) -> Result<Self, RetentionConfigError> {
        let seconds: u64 = match raw {
            Some(value) => value
                .parse()
                .map_err(|_| RetentionConfigError::InvalidInterval)?,
            None => 3600,
        };
        if !(60..=86_400).contains(&seconds) {
            return Err(RetentionConfigError::InvalidInterval);
        }
        Ok(Self {
            sweep_interval: Duration::from_secs(seconds),
        })
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SweepReport {
    pub runs: u64,
    pub messages: u64,
}

impl SweepReport {
    pub fn has_more(&self) -> bool {
        self.runs == 100 || self.messages == 500
    }
}

/// Deletes one bounded batch. Row locks let multiple workers sweep without duplicating work.
pub async fn sweep(pool: &PgPool) -> Result<SweepReport, sqlx::Error> {
    let mut tx = pool.begin().await?;
    let expired = sqlx::query(
        "SELECT r.id, r.tenant_id FROM runs r JOIN tenants t ON t.id = r.tenant_id \
         WHERE t.run_retention_days IS NOT NULL \
           AND r.status IN ('succeeded', 'failed', 'cancelled') \
           AND r.finished_at < now() - t.run_retention_days * interval '1 day' \
         ORDER BY r.finished_at LIMIT 100 FOR UPDATE OF r SKIP LOCKED",
    )
    .fetch_all(&mut *tx)
    .await?;
    for row in &expired {
        let tenant_id: Uuid = row.get("tenant_id");
        let run_id: Uuid = row.get("id");
        for query in [
            "DELETE FROM run_events WHERE tenant_id = $1 AND run_id = $2",
            "DELETE FROM node_runs WHERE tenant_id = $1 AND run_id = $2",
            "DELETE FROM jobs WHERE tenant_id = $1 AND run_id = $2",
            "DELETE FROM runs WHERE tenant_id = $1 AND id = $2",
        ] {
            sqlx::query(query)
                .bind(tenant_id)
                .bind(run_id)
                .execute(&mut *tx)
                .await?;
        }
    }
    let messages = sqlx::query(
        "WITH expired AS ( \
           SELECT m.id FROM messages m JOIN tenants t ON t.id = m.tenant_id \
           WHERE t.message_retention_days IS NOT NULL \
             AND m.created_at < now() - t.message_retention_days * interval '1 day' \
           ORDER BY m.created_at LIMIT 500 FOR UPDATE OF m SKIP LOCKED \
         ) DELETE FROM messages m USING expired WHERE m.id = expired.id",
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    tx.commit().await?;
    Ok(SweepReport {
        runs: expired.len() as u64,
        messages,
    })
}

#[cfg(test)]
mod tests {
    use super::{RetentionConfigError, RetentionSettings, SweepReport};
    use std::time::Duration;

    #[test]
    fn sweep_interval_is_bounded() {
        assert_eq!(
            RetentionSettings::parse(None).unwrap().sweep_interval,
            Duration::from_secs(3600)
        );
        assert_eq!(
            RetentionSettings::parse(Some("60")).unwrap().sweep_interval,
            Duration::from_secs(60)
        );
        for invalid in ["0", "59", "86401", "abc"] {
            assert_eq!(
                RetentionSettings::parse(Some(invalid)),
                Err(RetentionConfigError::InvalidInterval)
            );
        }
    }

    #[test]
    fn full_batches_request_another_sweep() {
        assert!(!SweepReport {
            runs: 99,
            messages: 499
        }
        .has_more());
        assert!(SweepReport {
            runs: 100,
            messages: 0
        }
        .has_more());
    }
}
