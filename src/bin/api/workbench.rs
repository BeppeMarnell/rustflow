use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    Json,
};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{postgres::PgRow, PgPool, Row};
use uuid::Uuid;

use super::{ensure_conversation_flow_access, principal, ApiError};

pub(super) async fn list_conversations(
    State(pool): State<PgPool>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let principal = principal(&pool, &headers, "runs:read").await?;
    let rows = sqlx::query(
        "SELECT c.id, c.created_at, latest.created_at AS last_run_at, \
         latest.flow_id, latest.flow_name, latest.status, preview.content AS preview, \
         (SELECT count(*) FROM runs counted WHERE counted.tenant_id = c.tenant_id AND counted.conversation_id = c.id) AS run_count \
         FROM conversations c \
         JOIN LATERAL ( \
           SELECT r.created_at, r.status, v.flow_id, f.name AS flow_name \
           FROM runs r \
           JOIN flow_versions v ON v.tenant_id = r.tenant_id AND v.id = r.flow_version_id \
           JOIN flows f ON f.tenant_id = v.tenant_id AND f.id = v.flow_id \
           WHERE r.tenant_id = c.tenant_id AND r.conversation_id = c.id \
           ORDER BY r.created_at DESC, r.id DESC LIMIT 1 \
         ) latest ON true \
         LEFT JOIN LATERAL ( \
           SELECT m.content FROM messages m \
           WHERE m.tenant_id = c.tenant_id AND m.conversation_id = c.id \
           ORDER BY m.created_at DESC, m.id DESC LIMIT 1 \
         ) preview ON true \
         WHERE c.tenant_id = $1 AND c.owner_api_key_id = $2 \
           AND NOT EXISTS ( \
             SELECT 1 FROM runs restricted \
             JOIN flow_versions definition ON definition.tenant_id = restricted.tenant_id AND definition.id = restricted.flow_version_id \
             WHERE restricted.tenant_id = c.tenant_id AND restricted.conversation_id = c.id \
               AND ( \
                 ($3::uuid[] IS NOT NULL AND NOT (definition.flow_id = ANY($3::uuid[]))) \
                 OR (NOT $4::boolean AND definition.definition->'nodes' @> '[{\"type\":\"rag.retrieve\"}]'::jsonb) \
               ) \
           ) \
         ORDER BY latest.created_at DESC, c.id DESC LIMIT 100",
    )
    .bind(principal.tenant_id)
    .bind(principal.key_id)
    .bind(principal.allowed_flow_ids.clone())
    .bind(principal.has_scope("rag:read"))
    .fetch_all(&pool)
    .await?;
    let conversations: Vec<_> = rows
        .into_iter()
        .map(|row| {
            json!({
                "conversation_id": row.get::<Uuid, _>("id"),
                "flow_id": row.get::<Uuid, _>("flow_id"),
                "flow_name": row.get::<String, _>("flow_name"),
                "status": row.get::<String, _>("status"),
                "preview": row.get::<Option<String>, _>("preview"),
                "run_count": row.get::<i64, _>("run_count"),
                "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
                "updated_at": row.get::<chrono::DateTime<chrono::Utc>, _>("last_run_at"),
            })
        })
        .collect();
    Ok(Json(json!({"conversations": conversations})))
}

pub(super) async fn list_conversation_runs(
    State(pool): State<PgPool>,
    Path(conversation_id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let principal = principal(&pool, &headers, "runs:read").await?;
    ensure_conversation_flow_access(&pool, &principal, conversation_id).await?;
    let rows = sqlx::query(
        "SELECT r.id, r.flow_version_id, v.flow_id, v.version, r.status, r.input, r.output, r.error, \
         r.created_at, r.started_at, r.finished_at \
         FROM runs r JOIN flow_versions v ON v.tenant_id = r.tenant_id AND v.id = r.flow_version_id \
         WHERE r.tenant_id = $1 AND r.conversation_id = $2 \
         ORDER BY r.created_at DESC, r.id DESC LIMIT 100",
    )
    .bind(principal.tenant_id)
    .bind(conversation_id)
    .fetch_all(&pool)
    .await?;
    let runs: Vec<_> = rows
        .into_iter()
        .map(|row| {
            json!({
                "run_id": row.get::<Uuid, _>("id"),
                "flow_version_id": row.get::<Uuid, _>("flow_version_id"),
                "flow_id": row.get::<Uuid, _>("flow_id"),
                "version": row.get::<i32, _>("version"),
                "status": row.get::<String, _>("status"),
                "input": row.get::<Value, _>("input"),
                "output": row.get::<Option<Value>, _>("output"),
                "error": row.get::<Option<Value>, _>("error"),
                "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
                "started_at": row.get::<Option<chrono::DateTime<chrono::Utc>>, _>("started_at"),
                "finished_at": row.get::<Option<chrono::DateTime<chrono::Utc>>, _>("finished_at"),
            })
        })
        .collect();
    Ok(Json(
        json!({"conversation_id": conversation_id, "runs": runs}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct NoteRequest {
    body: String,
}

fn validate_note(body: &str) -> Result<(), ApiError> {
    if body.trim().is_empty() || body.len() > 10_000 {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "note must contain 1 to 10000 UTF-8 bytes of text".into(),
        ));
    }
    Ok(())
}

fn note_json(row: &PgRow) -> Value {
    json!({
        "note_id": row.get::<Uuid, _>("id"),
        "body": row.get::<String, _>("body"),
        "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
        "updated_at": row.get::<chrono::DateTime<chrono::Utc>, _>("updated_at"),
    })
}

pub(super) async fn list_notes(
    State(pool): State<PgPool>,
    Path(conversation_id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let principal = principal(&pool, &headers, "runs:read").await?;
    ensure_conversation_flow_access(&pool, &principal, conversation_id).await?;
    let rows = sqlx::query("SELECT id, body, created_at, updated_at FROM conversation_notes WHERE tenant_id = $1 AND conversation_id = $2 ORDER BY created_at, id LIMIT 100")
        .bind(principal.tenant_id).bind(conversation_id).fetch_all(&pool).await?;
    Ok(Json(json!({
        "conversation_id": conversation_id,
        "notes": rows.iter().map(note_json).collect::<Vec<_>>()
    })))
}

pub(super) async fn create_note(
    State(pool): State<PgPool>,
    Path(conversation_id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<NoteRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let principal = principal(&pool, &headers, "runs:write").await?;
    ensure_conversation_flow_access(&pool, &principal, conversation_id).await?;
    validate_note(&body.body)?;
    let row = sqlx::query("INSERT INTO conversation_notes(tenant_id, conversation_id, author_api_key_id, body) VALUES ($1, $2, $3, $4) RETURNING id, body, created_at, updated_at")
        .bind(principal.tenant_id).bind(conversation_id).bind(principal.key_id).bind(body.body)
        .fetch_one(&pool).await?;
    Ok((StatusCode::CREATED, Json(note_json(&row))))
}

pub(super) async fn update_note(
    State(pool): State<PgPool>,
    Path((conversation_id, note_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
    Json(body): Json<NoteRequest>,
) -> Result<Json<Value>, ApiError> {
    let principal = principal(&pool, &headers, "runs:write").await?;
    ensure_conversation_flow_access(&pool, &principal, conversation_id).await?;
    validate_note(&body.body)?;
    let row = sqlx::query("UPDATE conversation_notes SET body = $4, updated_at = now() WHERE tenant_id = $1 AND conversation_id = $2 AND id = $3 RETURNING id, body, created_at, updated_at")
        .bind(principal.tenant_id).bind(conversation_id).bind(note_id).bind(body.body)
        .fetch_optional(&pool).await?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "note not found".into()))?;
    Ok(Json(note_json(&row)))
}

pub(super) async fn delete_note(
    State(pool): State<PgPool>,
    Path((conversation_id, note_id)): Path<(Uuid, Uuid)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let principal = principal(&pool, &headers, "runs:write").await?;
    ensure_conversation_flow_access(&pool, &principal, conversation_id).await?;
    let deleted = sqlx::query(
        "DELETE FROM conversation_notes WHERE tenant_id = $1 AND conversation_id = $2 AND id = $3",
    )
    .bind(principal.tenant_id)
    .bind(conversation_id)
    .bind(note_id)
    .execute(&pool)
    .await?
    .rows_affected();
    if deleted == 0 {
        return Err(ApiError(StatusCode::NOT_FOUND, "note not found".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::validate_note;

    #[test]
    fn notes_reject_empty_and_oversized_values() {
        assert!(validate_note("hello\nworld").is_ok());
        assert!(validate_note("\n \t").is_err());
        assert!(validate_note(&"x".repeat(10_001)).is_err());
        assert!(validate_note(&"é".repeat(5_001)).is_err());
        assert!(validate_note(&"x".repeat(10_000)).is_ok());
    }
}
