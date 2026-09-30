use chrono::{DateTime, Utc};
use serde::Serialize;
use serde_json::Value;
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize)]
pub struct RunEvent {
    pub run_id: Uuid,
    pub sequence: i64,
    pub node_id: Option<String>,
    pub event_type: String,
    pub status: Option<String>,
    pub metadata: Value,
    pub occurred_at: DateTime<Utc>,
}

pub async fn append(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    run_id: Uuid,
    node_id: Option<&str>,
    event_type: &str,
    status: Option<&str>,
    metadata: Value,
) -> Result<i64, sqlx::Error> {
    let sequence: i64 = sqlx::query_scalar(
        "UPDATE runs SET next_event_sequence = next_event_sequence + 1 WHERE tenant_id = $1 AND id = $2 RETURNING next_event_sequence - 1",
    )
    .bind(tenant_id).bind(run_id).fetch_one(&mut **tx).await?;
    sqlx::query("INSERT INTO run_events(run_id, sequence, tenant_id, node_id, event_type, status, metadata) VALUES ($1, $2, $3, $4, $5, $6, $7)")
        .bind(run_id).bind(sequence).bind(tenant_id).bind(node_id).bind(event_type).bind(status).bind(metadata)
        .execute(&mut **tx).await?;
    Ok(sequence)
}

pub async fn after(
    pool: &PgPool,
    tenant_id: Uuid,
    run_id: Uuid,
    after: i64,
) -> Result<Vec<RunEvent>, sqlx::Error> {
    let rows = sqlx::query("SELECT run_id, sequence, node_id, event_type, status, metadata, occurred_at FROM run_events WHERE tenant_id = $1 AND run_id = $2 AND sequence > $3 ORDER BY sequence LIMIT 100")
        .bind(tenant_id).bind(run_id).bind(after).fetch_all(pool).await?;
    rows.into_iter()
        .map(|row| {
            Ok(RunEvent {
                run_id: row.try_get("run_id")?,
                sequence: row.try_get("sequence")?,
                node_id: row.try_get("node_id")?,
                event_type: row.try_get("event_type")?,
                status: row.try_get("status")?,
                metadata: row.try_get("metadata")?,
                occurred_at: row.try_get("occurred_at")?,
            })
        })
        .collect()
}
