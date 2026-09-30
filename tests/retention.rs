use chatflow::retention;
use serde_json::json;
use sqlx::{PgPool, Row};
use uuid::Uuid;

async fn tenant_with_records(
    pool: &PgPool,
    retention_days: Option<i32>,
) -> (Uuid, Uuid, Uuid, Uuid) {
    let mut tx = pool.begin().await.unwrap();
    let tenant_id: Uuid = sqlx::query_scalar("INSERT INTO tenants(name, message_retention_days, run_retention_days) VALUES ($1, $2, $2) RETURNING id")
        .bind(format!("retention-{}", Uuid::new_v4()))
        .bind(retention_days)
        .fetch_one(&mut *tx).await.unwrap();
    let flow_id = Uuid::new_v4();
    sqlx::query("INSERT INTO flows(id, tenant_id, name) VALUES ($1, $2, 'retention test')")
        .bind(flow_id)
        .bind(tenant_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    let version_id: Uuid = sqlx::query_scalar("INSERT INTO flow_versions(tenant_id, flow_id, version, definition) VALUES ($1, $2, 1, $3) RETURNING id")
        .bind(tenant_id).bind(flow_id).bind(json!({})).fetch_one(&mut *tx).await.unwrap();
    let conversation_id: Uuid =
        sqlx::query_scalar("INSERT INTO conversations(tenant_id) VALUES ($1) RETURNING id")
            .bind(tenant_id)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    let old_message_id: Uuid = sqlx::query_scalar("INSERT INTO messages(tenant_id, conversation_id, role, content, created_at) VALUES ($1, $2, 'user', 'old', now() - interval '2 days') RETURNING id")
        .bind(tenant_id).bind(conversation_id).fetch_one(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO messages(tenant_id, conversation_id, role, content) VALUES ($1, $2, 'assistant', 'recent')")
        .bind(tenant_id).bind(conversation_id).execute(&mut *tx).await.unwrap();
    let old_run_id: Uuid = sqlx::query_scalar("INSERT INTO runs(tenant_id, conversation_id, flow_version_id, status, input, finished_at) VALUES ($1, $2, $3, 'succeeded', $4, now() - interval '2 days') RETURNING id")
        .bind(tenant_id).bind(conversation_id).bind(version_id).bind(json!({})).fetch_one(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO jobs(tenant_id, run_id, status) VALUES ($1, $2, 'done')")
        .bind(tenant_id)
        .bind(old_run_id)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("INSERT INTO node_runs(tenant_id, run_id, node_id, status, output, finished_at) VALUES ($1, $2, 'message', 'succeeded', $3, now() - interval '2 days')")
        .bind(tenant_id).bind(old_run_id).bind(json!({"value":"old"})).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO run_events(tenant_id, run_id, sequence, event_type) VALUES ($1, $2, 1, 'run.finished')")
        .bind(tenant_id).bind(old_run_id).execute(&mut *tx).await.unwrap();
    let active_run_id: Uuid = sqlx::query_scalar("INSERT INTO runs(tenant_id, conversation_id, flow_version_id, status, input, created_at) VALUES ($1, $2, $3, 'running', $4, now() - interval '2 days') RETURNING id")
        .bind(tenant_id).bind(conversation_id).bind(version_id).bind(json!({})).fetch_one(&mut *tx).await.unwrap();
    tx.commit().await.unwrap();
    (tenant_id, old_message_id, old_run_id, active_run_id)
}

#[tokio::test]
async fn retention_deletes_only_expired_configured_tenant_data() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let pool = PgPool::connect(&database_url).await.unwrap();
    sqlx::migrate!().run(&pool).await.unwrap();
    let (configured_tenant, old_message, old_run, active_run) =
        tenant_with_records(&pool, Some(1)).await;
    let (unconfigured_tenant, other_message, other_run, _) = tenant_with_records(&pool, None).await;

    let report = retention::sweep(&pool).await.unwrap();
    assert_eq!(report.runs, 1);
    assert_eq!(report.messages, 1);
    let counts = sqlx::query(
        "SELECT \
        (SELECT count(*) FROM runs WHERE tenant_id = $1 AND id = $2) AS old_run, \
        (SELECT count(*) FROM runs WHERE tenant_id = $1 AND id = $3) AS active_run, \
        (SELECT count(*) FROM run_events WHERE tenant_id = $1 AND run_id = $2) AS events, \
        (SELECT count(*) FROM node_runs WHERE tenant_id = $1 AND run_id = $2) AS nodes, \
        (SELECT count(*) FROM jobs WHERE tenant_id = $1 AND run_id = $2) AS jobs, \
        (SELECT count(*) FROM messages WHERE tenant_id = $1 AND id = $4) AS old_message, \
        (SELECT count(*) FROM messages WHERE tenant_id = $1) AS recent_messages",
    )
    .bind(configured_tenant)
    .bind(old_run)
    .bind(active_run)
    .bind(old_message)
    .fetch_one(&pool)
    .await
    .unwrap();
    for name in ["old_run", "events", "nodes", "jobs", "old_message"] {
        assert_eq!(counts.get::<i64, _>(name), 0, "{name}");
    }
    assert_eq!(counts.get::<i64, _>("active_run"), 1);
    assert_eq!(counts.get::<i64, _>("recent_messages"), 1);
    let other_counts = sqlx::query(
        "SELECT \
        (SELECT count(*) FROM runs WHERE tenant_id = $1 AND id = $2) AS run_count, \
        (SELECT count(*) FROM messages WHERE tenant_id = $1 AND id = $3) AS message_count",
    )
    .bind(unconfigured_tenant)
    .bind(other_run)
    .bind(other_message)
    .fetch_one(&pool)
    .await
    .unwrap();
    assert_eq!(other_counts.get::<i64, _>("run_count"), 1);
    assert_eq!(other_counts.get::<i64, _>("message_count"), 1);
}
