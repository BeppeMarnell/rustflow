use std::{convert::Infallible, time::Duration};

use async_stream::stream;
use axum::{
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use chatflow::{
    auth::{self, Principal},
    budget::{self, BudgetError, ModelPrices, Reservation},
    config::Config,
    events,
    flow::{self, FlowDefinition, ValueType},
    lifecycle::RunStatus,
    ragflow, runtime,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{postgres::PgPoolOptions, PgPool, Row};
use uuid::Uuid;

#[derive(Debug)]
struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(json!({"error": self.1}))).into_response()
    }
}

impl From<sqlx::Error> for ApiError {
    fn from(error: sqlx::Error) -> Self {
        tracing::error!(%error, "database request failed");
        Self(StatusCode::INTERNAL_SERVER_ERROR, "database error".into())
    }
}

async fn principal(pool: &PgPool, headers: &HeaderMap, scope: &str) -> Result<Principal, ApiError> {
    let bearer = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let principal = auth::authenticate(pool, bearer)
        .await?
        .ok_or_else(|| ApiError(StatusCode::UNAUTHORIZED, "invalid API key".into()))?;
    if !principal.has_scope(scope) {
        return Err(ApiError(StatusCode::FORBIDDEN, "missing scope".into()));
    }
    Ok(principal)
}

async fn lock_live_key(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    principal: &Principal,
) -> Result<(), ApiError> {
    let row = sqlx::query("SELECT public_id, primary_token_revoked_at FROM api_keys WHERE id = $1 AND tenant_id = $2 AND revoked_at IS NULL AND (expires_at IS NULL OR expires_at > now()) FOR UPDATE")
        .bind(principal.key_id).bind(principal.tenant_id).fetch_optional(&mut **tx).await?
        .ok_or_else(|| ApiError(StatusCode::UNAUTHORIZED, "API key is unavailable".into()))?;
    let active = if row.get::<String, _>("public_id") == principal.token_public_id {
        row.get::<Option<chrono::DateTime<chrono::Utc>>, _>("primary_token_revoked_at")
            .is_none()
    } else {
        sqlx::query_scalar::<_, bool>("SELECT EXISTS(SELECT 1 FROM api_key_tokens WHERE key_id = $1 AND public_id = $2 AND revoked_at IS NULL)")
            .bind(principal.key_id).bind(&principal.token_public_id).fetch_one(&mut **tx).await?
    };
    if !active {
        return Err(ApiError(
            StatusCode::UNAUTHORIZED,
            "API token was revoked".into(),
        ));
    }
    Ok(())
}

async fn rotate_key(
    State(pool): State<PgPool>,
    headers: HeaderMap,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let principal = principal(&pool, &headers, "keys:write").await?;
    let (token, public_id, digest) = auth::generate_key();
    let mut tx = pool.begin().await?;
    lock_live_key(&mut tx, &principal).await?;
    sqlx::query("INSERT INTO api_key_tokens(key_id, public_id, secret_digest) VALUES ($1, $2, $3)")
        .bind(principal.key_id)
        .bind(&public_id)
        .bind(digest)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"key_id":principal.key_id,"public_id":public_id,"api_key":token})),
    ))
}

async fn revoke_token(
    State(pool): State<PgPool>,
    Path(public_id): Path<String>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let principal = principal(&pool, &headers, "keys:write").await?;
    let mut tx = pool.begin().await?;
    lock_live_key(&mut tx, &principal).await?;
    let changed = if public_id
        == sqlx::query_scalar::<_, String>(
            "SELECT public_id FROM api_keys WHERE id = $1 AND tenant_id = $2",
        )
        .bind(principal.key_id)
        .bind(principal.tenant_id)
        .fetch_one(&mut *tx)
        .await?
    {
        sqlx::query("UPDATE api_keys SET primary_token_revoked_at = now() WHERE id = $1 AND tenant_id = $2 AND primary_token_revoked_at IS NULL")
            .bind(principal.key_id).bind(principal.tenant_id).execute(&mut *tx).await?.rows_affected()
    } else {
        sqlx::query("UPDATE api_key_tokens SET revoked_at = now() WHERE key_id = $1 AND public_id = $2 AND revoked_at IS NULL")
            .bind(principal.key_id).bind(&public_id).execute(&mut *tx).await?.rows_affected()
    };
    if changed == 0 {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            "API token not found".into(),
        ));
    }
    tx.commit().await?;
    Ok(Json(json!({"public_id":public_id,"revoked":true})))
}

async fn list_key_tokens(
    State(pool): State<PgPool>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let principal = principal(&pool, &headers, "keys:read").await?;
    let rows = sqlx::query("SELECT public_id, created_at, primary_token_revoked_at AS revoked_at FROM api_keys WHERE id = $1 AND tenant_id = $2 UNION ALL SELECT t.public_id, t.created_at, t.revoked_at FROM api_key_tokens t JOIN api_keys k ON k.id = t.key_id WHERE k.id = $1 AND k.tenant_id = $2 ORDER BY created_at")
        .bind(principal.key_id).bind(principal.tenant_id).fetch_all(&pool).await?;
    Ok(Json(
        json!({"key_id":principal.key_id,"tokens":rows.into_iter().map(|row| json!({"public_id":row.get::<String,_>("public_id"),"created_at":row.get::<chrono::DateTime<chrono::Utc>,_>("created_at"),"revoked_at":row.get::<Option<chrono::DateTime<chrono::Utc>>,_>("revoked_at")})).collect::<Vec<_>>()}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RagflowDatasetsRequest {
    dataset_ids: Vec<String>,
}

async fn list_ragflow_datasets(
    State(pool): State<PgPool>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let principal = principal(&pool, &headers, "rag:configure").await?;
    let ids = sqlx::query_scalar::<_, String>(
        "SELECT dataset_id FROM ragflow_datasets WHERE tenant_id = $1 ORDER BY dataset_id",
    )
    .bind(principal.tenant_id)
    .fetch_all(&pool)
    .await?;
    Ok(Json(json!({"dataset_ids":ids})))
}

async fn replace_ragflow_datasets(
    State(pool): State<PgPool>,
    headers: HeaderMap,
    Json(body): Json<RagflowDatasetsRequest>,
) -> Result<Json<Value>, ApiError> {
    let principal = principal(&pool, &headers, "rag:configure").await?;
    ragflow::validate_dataset_ids(&body.dataset_ids)
        .map_err(|error| ApiError(StatusCode::BAD_REQUEST, error.to_string()))?;
    let mut tx = pool.begin().await?;
    sqlx::query("SELECT id FROM tenants WHERE id = $1 FOR UPDATE")
        .bind(principal.tenant_id)
        .fetch_one(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM ragflow_datasets WHERE tenant_id = $1")
        .bind(principal.tenant_id)
        .execute(&mut *tx)
        .await?;
    for id in &body.dataset_ids {
        sqlx::query("INSERT INTO ragflow_datasets(tenant_id, dataset_id) VALUES ($1, $2)")
            .bind(principal.tenant_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(Json(json!({"dataset_ids":body.dataset_ids})))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PublishRequest {
    name: String,
    definition: FlowDefinition,
}

#[derive(Serialize)]
struct PublishedVersion {
    flow_id: Uuid,
    version_id: Uuid,
    version: i32,
}

async fn publish(
    State(pool): State<PgPool>,
    Path(flow_id): Path<Uuid>,
    headers: HeaderMap,
    Json(body): Json<PublishRequest>,
) -> Result<(StatusCode, Json<PublishedVersion>), ApiError> {
    let principal = principal(&pool, &headers, "flows:write").await?;
    if body.definition.flow_id != flow_id
        || !principal.allows_flow(flow_id)
        || body.name.trim().is_empty()
    {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "invalid flow ID or name".into(),
        ));
    }
    flow::validate(&body.definition)
        .map_err(|e| ApiError(StatusCode::UNPROCESSABLE_ENTITY, e.to_string()))?;
    validate_credentials(&pool, principal.tenant_id, &body.definition).await?;
    let mut tx = pool.begin().await?;
    sqlx::query(
        "INSERT INTO flows(id, tenant_id, name) VALUES ($1, $2, $3) ON CONFLICT (id) DO NOTHING",
    )
    .bind(flow_id)
    .bind(principal.tenant_id)
    .bind(&body.name)
    .execute(&mut *tx)
    .await?;
    let owner: Option<Uuid> =
        sqlx::query_scalar("SELECT tenant_id FROM flows WHERE id = $1 FOR UPDATE")
            .bind(flow_id)
            .fetch_optional(&mut *tx)
            .await?;
    if owner != Some(principal.tenant_id) {
        return Err(ApiError(StatusCode::NOT_FOUND, "flow not found".into()));
    }
    let version: i32 = sqlx::query_scalar("SELECT COALESCE(MAX(version), 0) + 1 FROM flow_versions WHERE tenant_id = $1 AND flow_id = $2")
        .bind(principal.tenant_id).bind(flow_id).fetch_one(&mut *tx).await?;
    let version_id = Uuid::new_v4();
    let definition = serde_json::to_value(body.definition)
        .map_err(|e| ApiError(StatusCode::BAD_REQUEST, e.to_string()))?;
    sqlx::query("INSERT INTO flow_versions(id, tenant_id, flow_id, version, definition) VALUES ($1, $2, $3, $4, $5)")
        .bind(version_id).bind(principal.tenant_id).bind(flow_id).bind(version).bind(definition).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(PublishedVersion {
            flow_id,
            version_id,
            version,
        }),
    ))
}

async fn validate_credentials(
    pool: &PgPool,
    tenant_id: Uuid,
    flow: &FlowDefinition,
) -> Result<(), ApiError> {
    for node in &flow.nodes {
        if node.kind != "llm.chat" {
            continue;
        }
        let alias = node.config["model_alias"].as_str().unwrap_or("");
        if alias == "mock" {
            continue;
        }
        let id = node.config["credential_id"]
            .as_str()
            .and_then(|value| Uuid::parse_str(value).ok())
            .ok_or_else(|| {
                ApiError(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "model credential ID is required".into(),
                )
            })?;
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM provider_credentials WHERE tenant_id = $1 AND id = $2 AND provider_alias = $3)")
            .bind(tenant_id).bind(id).bind(alias).fetch_one(pool).await?;
        if !exists {
            return Err(ApiError(
                StatusCode::UNPROCESSABLE_ENTITY,
                "model credential is unavailable".into(),
            ));
        }
    }
    Ok(())
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateCredentialRequest {
    provider_alias: String,
    secret_reference: String,
}

async fn register_credential(
    State(pool): State<PgPool>,
    headers: HeaderMap,
    Json(body): Json<CreateCredentialRequest>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let principal = principal(&pool, &headers, "credentials:write").await?;
    if body.provider_alias != "openai"
        || !chatflow::provider::valid_tenant_secret_reference(
            principal.tenant_id,
            &body.secret_reference,
        )
    {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid provider alias or Vault reference".into(),
        ));
    }
    let id: Uuid = sqlx::query_scalar("INSERT INTO provider_credentials(tenant_id, provider_alias, secret_reference) VALUES ($1, $2, $3) ON CONFLICT (tenant_id, provider_alias) DO UPDATE SET secret_reference = EXCLUDED.secret_reference RETURNING id")
        .bind(principal.tenant_id).bind(&body.provider_alias).bind(&body.secret_reference).fetch_one(&pool).await?;
    Ok((
        StatusCode::CREATED,
        Json(json!({"credential_id": id, "provider_alias": body.provider_alias})),
    ))
}

async fn list_credentials(
    State(pool): State<PgPool>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let principal = principal(&pool, &headers, "credentials:read").await?;
    let rows = sqlx::query("SELECT id, provider_alias FROM provider_credentials WHERE tenant_id = $1 ORDER BY provider_alias")
        .bind(principal.tenant_id).fetch_all(&pool).await?;
    Ok(Json(
        json!({"credentials": rows.into_iter().map(|row| json!({"credential_id": row.get::<Uuid,_>("id"), "provider_alias": row.get::<String,_>("provider_alias")})).collect::<Vec<_>>()}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DryRunRequest {
    definition: FlowDefinition,
    input: Value,
}

async fn dry_run_flow(
    State(pool): State<PgPool>,
    headers: HeaderMap,
    Json(body): Json<DryRunRequest>,
) -> Result<Json<Value>, ApiError> {
    let principal = principal(&pool, &headers, "flows:write").await?;
    if !principal.allows_flow(body.definition.flow_id) {
        return Err(ApiError(StatusCode::FORBIDDEN, "flow not allowed".into()));
    }
    flow::validate(&body.definition)
        .map_err(|error| ApiError(StatusCode::UNPROCESSABLE_ENTITY, error.to_string()))?;
    validate_run_input(&body.definition, &body.input)?;
    if body
        .definition
        .nodes
        .iter()
        .any(|node| node.kind == "rag.retrieve")
        && !principal.has_scope("rag:read")
    {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "missing rag:read scope".into(),
        ));
    }
    let result = runtime::dry_run(&pool, principal.tenant_id, &body.definition, &body.input)
        .await
        .map_err(|error| ApiError(StatusCode::UNPROCESSABLE_ENTITY, error.to_string()))?;
    Ok(Json(result))
}

async fn validate_flow(
    State(pool): State<PgPool>,
    headers: HeaderMap,
    Json(definition): Json<FlowDefinition>,
) -> Result<Json<Value>, ApiError> {
    let principal = principal(&pool, &headers, "flows:write").await?;
    if !principal.allows_flow(definition.flow_id) {
        return Err(ApiError(StatusCode::FORBIDDEN, "flow not allowed".into()));
    }
    flow::validate(&definition)
        .map_err(|error| ApiError(StatusCode::UNPROCESSABLE_ENTITY, error.to_string()))?;
    validate_credentials(&pool, principal.tenant_id, &definition).await?;
    Ok(Json(json!({"valid": true})))
}

async fn list_flows(
    State(pool): State<PgPool>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let principal = principal(&pool, &headers, "flows:read").await?;
    let rows = sqlx::query("SELECT f.id, f.name, MAX(v.version) AS latest_version FROM flows f LEFT JOIN flow_versions v ON v.flow_id = f.id AND v.tenant_id = f.tenant_id WHERE f.tenant_id = $1 GROUP BY f.id, f.name ORDER BY f.name, f.id")
        .bind(principal.tenant_id).fetch_all(&pool).await?;
    let flows: Vec<_> = rows
        .into_iter()
        .filter_map(|row| {
            let id: Uuid = row.get("id");
            principal.allows_flow(id).then(|| {
                json!({
                    "flow_id": id,
                    "name": row.get::<String, _>("name"),
                    "latest_version": row.get::<Option<i32>, _>("latest_version"),
                })
            })
        })
        .collect();
    Ok(Json(json!({"flows": flows})))
}

async fn list_versions(
    State(pool): State<PgPool>,
    Path(flow_id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let principal = principal(&pool, &headers, "flows:read").await?;
    if !principal.allows_flow(flow_id) {
        return Err(ApiError(StatusCode::FORBIDDEN, "flow not allowed".into()));
    }
    let exists: bool =
        sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM flows WHERE tenant_id = $1 AND id = $2)")
            .bind(principal.tenant_id)
            .bind(flow_id)
            .fetch_one(&pool)
            .await?;
    if !exists {
        return Err(ApiError(StatusCode::NOT_FOUND, "flow not found".into()));
    }
    let rows = sqlx::query("SELECT id, version, published_at FROM flow_versions WHERE tenant_id = $1 AND flow_id = $2 ORDER BY version DESC")
        .bind(principal.tenant_id).bind(flow_id).fetch_all(&pool).await?;
    let versions: Vec<_> = rows
        .into_iter()
        .map(|row| {
            json!({
                "version_id": row.get::<Uuid, _>("id"),
                "version": row.get::<i32, _>("version"),
                "published_at": row.get::<chrono::DateTime<chrono::Utc>, _>("published_at"),
            })
        })
        .collect();
    Ok(Json(json!({"flow_id": flow_id, "versions": versions})))
}

async fn get_version(
    State(pool): State<PgPool>,
    Path((flow_id, version)): Path<(Uuid, i32)>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let principal = principal(&pool, &headers, "flows:read").await?;
    if !principal.allows_flow(flow_id) {
        return Err(ApiError(StatusCode::FORBIDDEN, "flow not allowed".into()));
    }
    let row = sqlx::query("SELECT id, definition, published_at FROM flow_versions WHERE tenant_id = $1 AND flow_id = $2 AND version = $3")
        .bind(principal.tenant_id).bind(flow_id).bind(version).fetch_optional(&pool).await?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "flow version not found".into()))?;
    Ok(Json(json!({
        "flow_id": flow_id,
        "version": version,
        "version_id": row.get::<Uuid, _>("id"),
        "definition": row.get::<Value, _>("definition"),
        "published_at": row.get::<chrono::DateTime<chrono::Utc>, _>("published_at"),
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateRunRequest {
    flow_version_id: Uuid,
    conversation_id: Option<Uuid>,
    input: Value,
}

#[derive(Serialize)]
struct CreatedRun {
    run_id: Uuid,
    conversation_id: Uuid,
    flow_version_id: Uuid,
    status: String,
}

fn idempotency_key(headers: &HeaderMap) -> Result<Option<String>, ApiError> {
    let Some(raw) = headers.get("idempotency-key") else {
        return Ok(None);
    };
    let value = raw
        .to_str()
        .map_err(|_| ApiError(StatusCode::BAD_REQUEST, "invalid idempotency key".into()))?;
    if value.is_empty()
        || value.len() > 128
        || !value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b':'))
    {
        return Err(ApiError(
            StatusCode::BAD_REQUEST,
            "invalid idempotency key".into(),
        ));
    }
    Ok(Some(value.to_owned()))
}

async fn existing_run(
    pool: &PgPool,
    principal: &Principal,
    key: &str,
    body: &CreateRunRequest,
) -> Result<Option<CreatedRun>, ApiError> {
    let row = sqlx::query("SELECT r.id, r.conversation_id, r.flow_version_id, r.input, r.status FROM runs r JOIN conversations c ON c.tenant_id = r.tenant_id AND c.id = r.conversation_id WHERE r.tenant_id = $1 AND r.idempotency_key = $2 AND c.owner_api_key_id = $3")
        .bind(principal.tenant_id).bind(key).bind(principal.key_id).fetch_optional(pool).await?;
    let Some(row) = row else { return Ok(None) };
    let conversation_id: Uuid = row.try_get("conversation_id")?;
    let existing_version: Uuid = row.try_get("flow_version_id")?;
    let existing_input: Value = row.try_get("input")?;
    if existing_version != body.flow_version_id
        || existing_input != body.input
        || body.conversation_id.is_some_and(|id| id != conversation_id)
    {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "idempotency key was used for a different request".into(),
        ));
    }
    ensure_conversation_flow_access(pool, principal, conversation_id).await?;
    Ok(Some(CreatedRun {
        run_id: row.try_get("id")?,
        conversation_id,
        flow_version_id: existing_version,
        status: row.try_get("status")?,
    }))
}

#[tracing::instrument(skip_all, fields(flow_version_id = %body.flow_version_id, tenant_id, run_id))]
async fn create_run(
    State(pool): State<PgPool>,
    headers: HeaderMap,
    Json(body): Json<CreateRunRequest>,
) -> Result<(StatusCode, Json<CreatedRun>), ApiError> {
    let principal = principal(&pool, &headers, "runs:write").await?;
    tracing::Span::current().record("tenant_id", tracing::field::display(principal.tenant_id));
    let idempotency_key = idempotency_key(&headers)?;
    let row = sqlx::query(
        "SELECT flow_id, definition FROM flow_versions WHERE tenant_id = $1 AND id = $2",
    )
    .bind(principal.tenant_id)
    .bind(body.flow_version_id)
    .fetch_optional(&pool)
    .await?
    .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "flow version not found".into()))?;
    let flow_id: Uuid = row.try_get("flow_id")?;
    if !principal.allows_flow(flow_id) {
        return Err(ApiError(StatusCode::FORBIDDEN, "flow not allowed".into()));
    }
    let definition: Value = row.try_get("definition")?;
    let flow: FlowDefinition = serde_json::from_value(definition).map_err(|_| {
        ApiError(
            StatusCode::INTERNAL_SERVER_ERROR,
            "stored flow is invalid".into(),
        )
    })?;
    validate_run_input(&flow, &body.input)?;
    if flow.nodes.iter().any(|node| node.kind == "rag.retrieve") && !principal.has_scope("rag:read")
    {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "missing rag:read scope".into(),
        ));
    }
    for node in &flow.nodes {
        if node.kind == "llm.chat" {
            let alias = node
                .config
                .get("model_alias")
                .and_then(Value::as_str)
                .unwrap_or("");
            if !principal.allows_model(alias) {
                return Err(ApiError(StatusCode::FORBIDDEN, "model not allowed".into()));
            }
        }
    }
    if let Some(conversation_id) = body.conversation_id {
        ensure_conversation_flow_access(&pool, &principal, conversation_id).await?;
    }
    if let Some(key) = &idempotency_key {
        if let Some(existing) = existing_run(&pool, &principal, key, &body).await? {
            return Ok((StatusCode::OK, Json(existing)));
        }
    }
    let mut tx = pool.begin().await?;
    let prices = if flow
        .nodes
        .iter()
        .any(|node| node.kind == "llm.chat" && node.config["model_alias"] == "openai")
    {
        Some(ModelPrices::from_env().map_err(budget_api_error)?)
    } else {
        None
    };
    let reservation = budget::reserve(&flow, prices).map_err(budget_api_error)?;
    if let Err(error) = enforce_run_quotas(&mut tx, &principal, reservation).await {
        tx.rollback().await?;
        if let Some(key) = &idempotency_key {
            if let Some(existing) = existing_run(&pool, &principal, key, &body).await? {
                return Ok((StatusCode::OK, Json(existing)));
            }
        }
        return Err(error);
    }
    let conversation_id = if let Some(id) = body.conversation_id {
        let exists: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM conversations WHERE tenant_id = $1 AND id = $2 AND owner_api_key_id = $3)",
        )
        .bind(principal.tenant_id)
        .bind(id)
        .bind(principal.key_id)
        .fetch_one(&mut *tx)
        .await?;
        if !exists {
            return Err(ApiError(
                StatusCode::NOT_FOUND,
                "conversation not found".into(),
            ));
        }
        id
    } else {
        sqlx::query_scalar(
            "INSERT INTO conversations(tenant_id, owner_api_key_id) VALUES ($1, $2) RETURNING id",
        )
        .bind(principal.tenant_id)
        .bind(principal.key_id)
        .fetch_one(&mut *tx)
        .await?
    };
    if let Some(message) = body.input.get("message").and_then(Value::as_str) {
        sqlx::query("INSERT INTO messages(tenant_id, conversation_id, role, content) VALUES ($1, $2, 'user', $3)")
            .bind(principal.tenant_id).bind(conversation_id).bind(message).execute(&mut *tx).await?;
    }
    let run_id: Option<Uuid> = sqlx::query_scalar("INSERT INTO runs(tenant_id, conversation_id, flow_version_id, status, input, idempotency_key, api_key_id, reserved_output_tokens, reserved_input_tokens, reserved_spend_microusd) VALUES ($1, $2, $3, 'queued', $4, $5, $6, $7, $8, $9) ON CONFLICT (tenant_id, idempotency_key) DO NOTHING RETURNING id")
        .bind(principal.tenant_id).bind(conversation_id).bind(body.flow_version_id).bind(&body.input).bind(&idempotency_key).bind(principal.key_id).bind(reservation.output_tokens).bind(reservation.input_tokens).bind(reservation.spend_microusd).fetch_optional(&mut *tx).await?;
    let Some(run_id) = run_id else {
        tx.rollback().await?;
        let existing = existing_run(
            &pool,
            &principal,
            idempotency_key.as_deref().expect("conflict requires key"),
            &body,
        )
        .await?
        .ok_or_else(|| {
            ApiError(
                StatusCode::CONFLICT,
                "idempotency key is unavailable".into(),
            )
        })?;
        return Ok((StatusCode::OK, Json(existing)));
    };
    sqlx::query("INSERT INTO jobs(tenant_id, run_id, status, idempotency_key) VALUES ($1, $2, 'queued', $3)")
        .bind(principal.tenant_id)
        .bind(run_id)
        .bind(&idempotency_key)
        .execute(&mut *tx)
        .await?;
    events::append(
        &mut tx,
        principal.tenant_id,
        run_id,
        None,
        "run.queued",
        Some("queued"),
        json!({}),
    )
    .await?;
    tx.commit().await?;
    tracing::Span::current().record("run_id", tracing::field::display(run_id));
    chatflow::telemetry::record_run_created();
    Ok((
        StatusCode::CREATED,
        Json(CreatedRun {
            run_id,
            conversation_id,
            flow_version_id: body.flow_version_id,
            status: "queued".into(),
        }),
    ))
}

fn budget_api_error(error: BudgetError) -> ApiError {
    let status = match error {
        BudgetError::MissingOrInvalidPrices => StatusCode::SERVICE_UNAVAILABLE,
        BudgetError::UnknownModel(_) | BudgetError::InvalidConfig => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
        BudgetError::Overflow => StatusCode::INTERNAL_SERVER_ERROR,
    };
    ApiError(status, error.to_string())
}

fn token_budget_exceeded(used: i64, reservation: i64, limit: i64) -> bool {
    used.saturating_add(reservation) > limit
}

fn quota_exceeded(active: i64, recent: i64, max_active: i32, max_recent: i32) -> bool {
    active >= i64::from(max_active) || recent >= i64::from(max_recent)
}

async fn enforce_run_quotas(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    principal: &Principal,
    reservation: Reservation,
) -> Result<(), ApiError> {
    // Every run creator locks the tenant first, then its key. This serializes quota checks
    // across API processes without relying on an in-memory counter.
    let tenant = sqlx::query(
        "SELECT max_concurrent_runs, max_runs_per_minute, max_output_tokens_per_day, max_input_tokens_per_day, max_spend_microusd_per_day FROM tenants WHERE id = $1 FOR UPDATE",
    )
    .bind(principal.tenant_id)
    .fetch_one(&mut **tx)
    .await?;
    let key = sqlx::query("SELECT max_concurrent_runs, max_runs_per_minute, max_output_tokens_per_day, max_input_tokens_per_day, max_spend_microusd_per_day FROM api_keys WHERE id = $1 AND tenant_id = $2 AND revoked_at IS NULL FOR UPDATE")
        .bind(principal.key_id).bind(principal.tenant_id).fetch_optional(&mut **tx).await?
        .ok_or_else(|| ApiError(StatusCode::UNAUTHORIZED, "API key was revoked".into()))?;
    let row = sqlx::query("SELECT COUNT(*) FILTER (WHERE status IN ('queued', 'running')) AS active, COUNT(*) FILTER (WHERE created_at > now() - interval '1 minute') AS recent FROM runs WHERE tenant_id = $1")
        .bind(principal.tenant_id).fetch_one(&mut **tx).await?;
    if quota_exceeded(
        row.get("active"),
        row.get("recent"),
        tenant.get("max_concurrent_runs"),
        tenant.get("max_runs_per_minute"),
    ) {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "tenant run quota exceeded".into(),
        ));
    }
    let row = sqlx::query("SELECT COUNT(*) FILTER (WHERE status IN ('queued', 'running')) AS active, COUNT(*) FILTER (WHERE created_at > now() - interval '1 minute') AS recent FROM runs WHERE api_key_id = $1")
        .bind(principal.key_id).fetch_one(&mut **tx).await?;
    if quota_exceeded(
        row.get("active"),
        row.get("recent"),
        key.get("max_concurrent_runs"),
        key.get("max_runs_per_minute"),
    ) {
        return Err(ApiError(
            StatusCode::TOO_MANY_REQUESTS,
            "API key run quota exceeded".into(),
        ));
    }
    let tenant_reserved = sqlx::query("SELECT COALESCE(SUM(reserved_output_tokens), 0)::bigint AS output_tokens, COALESCE(SUM(reserved_input_tokens), 0)::bigint AS input_tokens, COALESCE(SUM(reserved_spend_microusd), 0)::bigint AS spend FROM runs WHERE tenant_id = $1 AND created_at > now() - interval '1 day'")
        .bind(principal.tenant_id).fetch_one(&mut **tx).await?;
    let key_reserved = sqlx::query("SELECT COALESCE(SUM(reserved_output_tokens), 0)::bigint AS output_tokens, COALESCE(SUM(reserved_input_tokens), 0)::bigint AS input_tokens, COALESCE(SUM(reserved_spend_microusd), 0)::bigint AS spend FROM runs WHERE api_key_id = $1 AND created_at > now() - interval '1 day'")
        .bind(principal.key_id).fetch_one(&mut **tx).await?;
    for (label, limits, used) in [
        ("tenant", &tenant, &tenant_reserved),
        ("API key", &key, &key_reserved),
    ] {
        for (kind, field, limit_field, new_amount) in [
            (
                "output token",
                "output_tokens",
                "max_output_tokens_per_day",
                reservation.output_tokens,
            ),
            (
                "input token",
                "input_tokens",
                "max_input_tokens_per_day",
                reservation.input_tokens,
            ),
            (
                "spend",
                "spend",
                "max_spend_microusd_per_day",
                reservation.spend_microusd,
            ),
        ] {
            if token_budget_exceeded(used.get(field), new_amount, limits.get(limit_field)) {
                return Err(ApiError(
                    StatusCode::TOO_MANY_REQUESTS,
                    format!("{label} {kind} budget exceeded"),
                ));
            }
        }
    }
    Ok(())
}

fn validate_run_input(flow: &FlowDefinition, input: &Value) -> Result<(), ApiError> {
    let object = input.as_object().ok_or_else(|| {
        ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "input must be an object".into(),
        )
    })?;
    if object.len() != flow.inputs.len() {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "input fields do not match flow".into(),
        ));
    }
    for (name, value_type) in &flow.inputs {
        let value = object.get(name).ok_or_else(|| {
            ApiError(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("missing input: {name}"),
            )
        })?;
        let valid = match value_type {
            ValueType::String => value.is_string(),
            ValueType::Boolean => value.is_boolean(),
            ValueType::Object => value.is_object(),
        };
        if !valid {
            return Err(ApiError(
                StatusCode::UNPROCESSABLE_ENTITY,
                format!("invalid input: {name}"),
            ));
        }
    }
    Ok(())
}

async fn get_run(
    State(pool): State<PgPool>,
    Path(run_id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let principal = principal(&pool, &headers, "runs:read").await?;
    ensure_run_flow_access(&pool, &principal, run_id, true).await?;
    let row = sqlx::query("SELECT id, conversation_id, flow_version_id, status, output, error, created_at, started_at, finished_at FROM runs WHERE tenant_id = $1 AND id = $2")
        .bind(principal.tenant_id).bind(run_id).fetch_optional(&pool).await?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "run not found".into()))?;
    Ok(Json(json!({
        "run_id": row.try_get::<Uuid, _>("id")?,
        "conversation_id": row.try_get::<Uuid, _>("conversation_id")?,
        "flow_version_id": row.try_get::<Uuid, _>("flow_version_id")?,
        "status": row.try_get::<String, _>("status")?,
        "output": row.try_get::<Option<Value>, _>("output")?,
        "error": row.try_get::<Option<Value>, _>("error")?,
        "created_at": row.try_get::<chrono::DateTime<chrono::Utc>, _>("created_at")?,
        "started_at": row.try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("started_at")?,
        "finished_at": row.try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("finished_at")?,
    })))
}

async fn run_timeline(
    State(pool): State<PgPool>,
    Path(run_id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let principal = principal(&pool, &headers, "runs:read").await?;
    ensure_run_flow_access(&pool, &principal, run_id, true).await?;
    let mut sequence = 0;
    let mut timeline = Vec::new();
    loop {
        let batch = events::after(&pool, principal.tenant_id, run_id, sequence).await?;
        let count = batch.len();
        if let Some(last) = batch.last() {
            sequence = last.sequence;
        }
        timeline.extend(batch);
        if count < 100 {
            break;
        }
    }
    let rows = sqlx::query("SELECT node_id, status, attempt, output, error, started_at, finished_at FROM node_runs WHERE tenant_id = $1 AND run_id = $2 ORDER BY started_at, node_id, attempt")
        .bind(principal.tenant_id).bind(run_id).fetch_all(&pool).await?;
    let nodes: Vec<_> = rows
        .into_iter()
        .map(|row| {
            json!({
                "node_id": row.get::<String, _>("node_id"),
                "status": row.get::<String, _>("status"),
                "attempt": row.get::<i32, _>("attempt"),
                "output": row.get::<Option<Value>, _>("output"),
                "error": row.get::<Option<Value>, _>("error"),
                "started_at": row.get::<chrono::DateTime<chrono::Utc>, _>("started_at"),
                "finished_at": row.get::<Option<chrono::DateTime<chrono::Utc>>, _>("finished_at"),
            })
        })
        .collect();
    Ok(Json(
        json!({"run_id": run_id, "events": timeline, "nodes": nodes}),
    ))
}

async fn cancel_run(
    State(pool): State<PgPool>,
    Path(run_id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let principal = principal(&pool, &headers, "runs:write").await?;
    ensure_run_flow_access(&pool, &principal, run_id, false).await?;
    let mut tx = pool.begin().await?;
    let job_status: String = sqlx::query_scalar(
        "SELECT status FROM jobs WHERE tenant_id = $1 AND run_id = $2 FOR UPDATE",
    )
    .bind(principal.tenant_id)
    .bind(run_id)
    .fetch_one(&mut *tx)
    .await?;
    let run_status: String =
        sqlx::query_scalar("SELECT status FROM runs WHERE tenant_id = $1 AND id = $2 FOR UPDATE")
            .bind(principal.tenant_id)
            .bind(run_id)
            .fetch_one(&mut *tx)
            .await?;
    if !RunStatus::parse(&run_status).is_some_and(RunStatus::can_cancel)
        || !matches!(job_status.as_str(), "queued" | "leased")
    {
        return Err(ApiError(
            StatusCode::CONFLICT,
            "run is already terminal".into(),
        ));
    }
    sqlx::query("UPDATE jobs SET status = 'cancelled', lease_owner = NULL, lease_expires_at = NULL WHERE tenant_id = $1 AND run_id = $2")
        .bind(principal.tenant_id).bind(run_id).execute(&mut *tx).await?;
    sqlx::query("UPDATE runs SET status = 'cancelled', finished_at = now() WHERE tenant_id = $1 AND id = $2")
        .bind(principal.tenant_id).bind(run_id).execute(&mut *tx).await?;
    events::append(
        &mut tx,
        principal.tenant_id,
        run_id,
        None,
        "run.cancelled",
        Some("cancelled"),
        json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({"run_id": run_id, "status": "cancelled"})))
}

async fn stream_events(
    State(pool): State<PgPool>,
    Path(run_id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Sse<impl futures_util::Stream<Item = Result<Event, Infallible>>>, ApiError> {
    let principal = principal(&pool, &headers, "runs:read").await?;
    ensure_run_flow_access(&pool, &principal, run_id, true).await?;
    let after = headers
        .get("last-event-id")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<i64>().ok())
        .unwrap_or(0)
        .max(0);
    let output = stream! {
        let mut sequence = after;
        loop {
            match events::after(&pool, principal.tenant_id, run_id, sequence).await {
                Ok(events) => {
                    for event in events {
                        sequence = event.sequence;
                        let body = serde_json::to_string(&event).expect("event serializes");
                        yield Ok(Event::default().id(sequence.to_string()).event(&event.event_type).data(body));
                    }
                }
                Err(error) => {
                    tracing::error!(%error, %run_id, "event stream failed");
                    yield Ok(Event::default().event("error").data("event stream failed"));
                    break;
                }
            }
            let status: Result<String, _> = sqlx::query_scalar("SELECT status FROM runs WHERE tenant_id = $1 AND id = $2")
                .bind(principal.tenant_id).bind(run_id).fetch_one(&pool).await;
            match status {
                Ok(status) if matches!(status.as_str(), "succeeded" | "failed" | "cancelled") => {
                    let tail = events::after(&pool, principal.tenant_id, run_id, sequence).await.unwrap_or_default();
                    if tail.is_empty() { break; }
                }
                Err(_) => break,
                _ => {}
            }
            tokio::time::sleep(Duration::from_millis(200)).await;
        }
    };
    Ok(Sse::new(output).keep_alive(KeepAlive::default()))
}

async fn messages(
    State(pool): State<PgPool>,
    Path(conversation_id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let principal = principal(&pool, &headers, "runs:read").await?;
    ensure_conversation_flow_access(&pool, &principal, conversation_id).await?;
    let rows = sqlx::query("SELECT id, role, content, created_at FROM messages WHERE tenant_id = $1 AND conversation_id = $2 ORDER BY created_at, id")
        .bind(principal.tenant_id).bind(conversation_id).fetch_all(&pool).await?;
    let list: Vec<_> = rows
        .into_iter()
        .map(|row| {
            json!({
                "id": row.get::<Uuid, _>("id"),
                "role": row.get::<String, _>("role"),
                "content": row.get::<String, _>("content"),
                "created_at": row.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
            })
        })
        .collect();
    Ok(Json(
        json!({"conversation_id": conversation_id, "messages": list}),
    ))
}

async fn ensure_conversation_flow_access(
    pool: &PgPool,
    principal: &Principal,
    conversation_id: Uuid,
) -> Result<(), ApiError> {
    require_conversation_owner(pool, principal, conversation_id).await?;
    let definitions = sqlx::query("SELECT DISTINCT v.flow_id, v.definition FROM runs r JOIN flow_versions v ON v.id = r.flow_version_id AND v.tenant_id = r.tenant_id WHERE r.tenant_id = $1 AND r.conversation_id = $2")
        .bind(principal.tenant_id).bind(conversation_id).fetch_all(pool).await?;
    for definition in definitions {
        let flow_id: Uuid = definition.try_get("flow_id")?;
        if !principal.allows_flow(flow_id) {
            return Err(ApiError(StatusCode::FORBIDDEN, "flow not allowed".into()));
        }
        let flow: Value = definition.try_get("definition")?;
        if flow_uses_rag(&flow) && !principal.has_scope("rag:read") {
            return Err(ApiError(
                StatusCode::FORBIDDEN,
                "missing rag:read scope".into(),
            ));
        }
    }
    Ok(())
}

async fn require_conversation_owner(
    pool: &PgPool,
    principal: &Principal,
    conversation_id: Uuid,
) -> Result<(), ApiError> {
    let owned: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM conversations WHERE tenant_id = $1 AND id = $2 AND owner_api_key_id = $3)")
        .bind(principal.tenant_id).bind(conversation_id).bind(principal.key_id)
        .fetch_one(pool).await?;
    if !owned {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            "conversation not found".into(),
        ));
    }
    Ok(())
}

async fn list_variables(
    State(pool): State<PgPool>,
    Path(conversation_id): Path<Uuid>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    let principal = principal(&pool, &headers, "runs:read").await?;
    ensure_conversation_flow_access(&pool, &principal, conversation_id).await?;
    let rows = sqlx::query("SELECT name, value FROM conversation_variables WHERE tenant_id = $1 AND conversation_id = $2 ORDER BY name")
        .bind(principal.tenant_id).bind(conversation_id).fetch_all(&pool).await?;
    let variables = rows
        .into_iter()
        .map(|row| (row.get::<String, _>("name"), row.get::<Value, _>("value")))
        .collect::<serde_json::Map<String, Value>>();
    Ok(Json(
        json!({"conversation_id": conversation_id, "variables": variables}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PutVariableRequest {
    value: Value,
}

async fn put_variable(
    State(pool): State<PgPool>,
    Path((conversation_id, name)): Path<(Uuid, String)>,
    headers: HeaderMap,
    Json(body): Json<PutVariableRequest>,
) -> Result<Json<Value>, ApiError> {
    let principal = principal(&pool, &headers, "runs:write").await?;
    if !flow::valid_variable_name(&name)
        || serde_json::to_vec(&body.value)
            .expect("JSON value serializes")
            .len()
            > 16_384
    {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "invalid variable name or value size".into(),
        ));
    }
    ensure_conversation_flow_access(&pool, &principal, conversation_id).await?;
    let mut tx = pool.begin().await?;
    let owned: Option<Uuid> = sqlx::query_scalar("SELECT id FROM conversations WHERE tenant_id = $1 AND id = $2 AND owner_api_key_id = $3 FOR UPDATE")
        .bind(principal.tenant_id).bind(conversation_id).bind(principal.key_id)
        .fetch_optional(&mut *tx).await?;
    if owned.is_none() {
        return Err(ApiError(
            StatusCode::NOT_FOUND,
            "conversation not found".into(),
        ));
    }
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM conversation_variables WHERE tenant_id = $1 AND conversation_id = $2",
    )
    .bind(principal.tenant_id)
    .bind(conversation_id)
    .fetch_one(&mut *tx)
    .await?;
    let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM conversation_variables WHERE tenant_id = $1 AND conversation_id = $2 AND name = $3)")
        .bind(principal.tenant_id).bind(conversation_id).bind(&name).fetch_one(&mut *tx).await?;
    if !exists && count >= 64 {
        return Err(ApiError(
            StatusCode::UNPROCESSABLE_ENTITY,
            "conversation variable limit reached".into(),
        ));
    }
    sqlx::query("INSERT INTO conversation_variables(tenant_id, conversation_id, name, value) VALUES ($1, $2, $3, $4) ON CONFLICT (tenant_id, conversation_id, name) DO UPDATE SET value = EXCLUDED.value, updated_at = now()")
        .bind(principal.tenant_id).bind(conversation_id).bind(&name).bind(&body.value)
        .execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(Json(
        json!({"conversation_id": conversation_id, "name": name, "value": body.value}),
    ))
}

async fn delete_variable(
    State(pool): State<PgPool>,
    Path((conversation_id, name)): Path<(Uuid, String)>,
    headers: HeaderMap,
) -> Result<StatusCode, ApiError> {
    let principal = principal(&pool, &headers, "runs:write").await?;
    ensure_conversation_flow_access(&pool, &principal, conversation_id).await?;
    if !flow::valid_variable_name(&name) {
        return Err(ApiError(StatusCode::NOT_FOUND, "variable not found".into()));
    }
    let changed = sqlx::query("DELETE FROM conversation_variables WHERE tenant_id = $1 AND conversation_id = $2 AND name = $3")
        .bind(principal.tenant_id).bind(conversation_id).bind(&name).execute(&pool).await?;
    if changed.rows_affected() == 0 {
        return Err(ApiError(StatusCode::NOT_FOUND, "variable not found".into()));
    }
    Ok(StatusCode::NO_CONTENT)
}

fn flow_uses_rag(definition: &Value) -> bool {
    definition
        .get("nodes")
        .and_then(Value::as_array)
        .is_some_and(|nodes| {
            nodes
                .iter()
                .any(|node| node.get("type").and_then(Value::as_str) == Some("rag.retrieve"))
        })
}

async fn ensure_run_flow_access(
    pool: &PgPool,
    principal: &Principal,
    run_id: Uuid,
    require_document_scope: bool,
) -> Result<(), ApiError> {
    let row = sqlx::query("SELECT v.flow_id, v.definition FROM runs r JOIN flow_versions v ON v.id = r.flow_version_id AND v.tenant_id = r.tenant_id JOIN conversations c ON c.tenant_id = r.tenant_id AND c.id = r.conversation_id WHERE r.tenant_id = $1 AND r.id = $2 AND c.owner_api_key_id = $3")
        .bind(principal.tenant_id).bind(run_id).bind(principal.key_id).fetch_optional(pool).await?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, "run not found".into()))?;
    let flow_id: Uuid = row.try_get("flow_id")?;
    if !principal.allows_flow(flow_id) {
        return Err(ApiError(StatusCode::FORBIDDEN, "flow not allowed".into()));
    }
    let definition: Value = row.try_get("definition")?;
    if require_document_scope && flow_uses_rag(&definition) && !principal.has_scope("rag:read") {
        return Err(ApiError(
            StatusCode::FORBIDDEN,
            "missing rag:read scope".into(),
        ));
    }
    Ok(())
}

#[derive(Debug)]
struct OpsMetrics {
    queued_runs: i64,
    oldest_run_queue_seconds: f64,
    model_failures_5m: i64,
    provider_throttles_5m: i64,
    input_tokens_5m: i64,
    output_tokens_5m: i64,
}

fn format_ops_metrics(tenant_id: Uuid, m: &OpsMetrics, cost_usd_5m: Option<f64>) -> String {
    let label = format!("{{tenant_id=\"{tenant_id}\"}}");
    let mut output = format!(
        "# TYPE chatflow_queued_runs gauge\nchatflow_queued_runs{label} {}\n\
         # TYPE chatflow_oldest_run_queue_seconds gauge\nchatflow_oldest_run_queue_seconds{label} {}\n\
         # TYPE chatflow_model_failures_5m gauge\nchatflow_model_failures_5m{label} {}\n\
         # TYPE chatflow_provider_throttles_5m gauge\nchatflow_provider_throttles_5m{label} {}\n\
         # TYPE chatflow_input_tokens_5m gauge\nchatflow_input_tokens_5m{label} {}\n\
         # TYPE chatflow_output_tokens_5m gauge\nchatflow_output_tokens_5m{label} {}\n",
        m.queued_runs, m.oldest_run_queue_seconds,
        m.model_failures_5m, m.provider_throttles_5m, m.input_tokens_5m, m.output_tokens_5m,
    );
    if let Some(cost) = cost_usd_5m {
        output.push_str(&format!("# TYPE chatflow_model_estimated_cost_usd_5m gauge\nchatflow_model_estimated_cost_usd_5m{label} {cost}\n"));
    }
    output
}

fn estimated_model_cost(input_tokens: i64, output_tokens: i64) -> Option<f64> {
    let prices = ModelPrices::from_env().ok()?;
    Some(
        (input_tokens as f64 * prices.input_usd_per_million
            + output_tokens as f64 * prices.output_usd_per_million)
            / 1_000_000.0,
    )
}

async fn ops_metrics(
    State(pool): State<PgPool>,
    headers: HeaderMap,
) -> Result<([(&'static str, &'static str); 1], String), ApiError> {
    let principal = principal(&pool, &headers, "ops:read").await?;
    let queue = sqlx::query("SELECT COUNT(*) AS queued, COALESCE(EXTRACT(EPOCH FROM now() - MIN(created_at)), 0)::float8 AS oldest FROM jobs WHERE tenant_id = $1 AND status = 'queued'")
        .bind(principal.tenant_id).fetch_one(&pool).await?;
    let failures: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM run_events WHERE tenant_id = $1 AND event_type = 'model.failure' AND occurred_at > now() - interval '5 minutes'")
        .bind(principal.tenant_id).fetch_one(&pool).await?;
    let throttles: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM run_events WHERE tenant_id = $1 AND event_type = 'model.failure' AND metadata->>'throttled' = 'true' AND occurred_at > now() - interval '5 minutes'")
        .bind(principal.tenant_id).fetch_one(&pool).await?;
    let usage = sqlx::query("SELECT COALESCE(SUM((metadata->>'input_tokens')::bigint), 0)::bigint AS input_tokens, COALESCE(SUM((metadata->>'output_tokens')::bigint), 0)::bigint AS output_tokens FROM run_events WHERE tenant_id = $1 AND event_type = 'model.usage' AND occurred_at > now() - interval '5 minutes'")
        .bind(principal.tenant_id).fetch_one(&pool).await?;
    let metered_usage = sqlx::query("SELECT COALESCE(SUM((metadata->>'input_tokens')::bigint), 0)::bigint AS input_tokens, COALESCE(SUM((metadata->>'output_tokens')::bigint), 0)::bigint AS output_tokens FROM run_events WHERE tenant_id = $1 AND event_type = 'model.usage' AND metadata->>'model_alias' = 'openai' AND occurred_at > now() - interval '5 minutes'")
        .bind(principal.tenant_id).fetch_one(&pool).await?;
    let metrics = OpsMetrics {
        queued_runs: queue.get("queued"),
        oldest_run_queue_seconds: queue.get("oldest"),
        model_failures_5m: failures,
        provider_throttles_5m: throttles,
        input_tokens_5m: usage.get("input_tokens"),
        output_tokens_5m: usage.get("output_tokens"),
    };
    let cost = estimated_model_cost(
        metered_usage.get("input_tokens"),
        metered_usage.get("output_tokens"),
    );
    Ok((
        [("content-type", "text/plain; version=0.0.4; charset=utf-8")],
        format_ops_metrics(principal.tenant_id, &metrics, cost),
    ))
}

async fn live() -> Json<Value> {
    Json(json!({"status": "ok"}))
}

async fn ready(State(pool): State<PgPool>) -> Result<Json<Value>, StatusCode> {
    sqlx::query_scalar::<_, i32>("SELECT 1")
        .fetch_one(&pool)
        .await
        .map_err(|_| StatusCode::SERVICE_UNAVAILABLE)?;
    Ok(Json(json!({"status": "ready"})))
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let _telemetry = chatflow::telemetry::init("chatflow-api")?;
    let config = Config::from_env()?;
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect(&config.database_url)
        .await?;
    sqlx::migrate!().run(&pool).await?;
    let app = Router::new()
        .route("/health/live", get(live))
        .route("/health/ready", get(ready))
        .route("/v1/flows", get(list_flows))
        .route("/v1/api-keys/rotate", post(rotate_key))
        .route("/v1/api-keys/tokens", get(list_key_tokens))
        .route("/v1/api-keys/tokens/{public_id}/revoke", post(revoke_token))
        .route(
            "/v1/ragflow-datasets",
            get(list_ragflow_datasets).put(replace_ragflow_datasets),
        )
        .route(
            "/v1/provider-credentials",
            post(register_credential).get(list_credentials),
        )
        .route("/v1/flows/validate", post(validate_flow))
        .route("/v1/flows/dry-run", post(dry_run_flow))
        .route("/v1/flows/{flow_id}/versions", post(publish))
        .route("/v1/flows/{flow_id}/versions", get(list_versions))
        .route("/v1/flows/{flow_id}/versions/{version}", get(get_version))
        .route("/v1/ops/metrics", get(ops_metrics))
        .route("/v1/runs", post(create_run))
        .route("/v1/runs/{run_id}", get(get_run))
        .route("/v1/runs/{run_id}/timeline", get(run_timeline))
        .route("/v1/runs/{run_id}/cancel", post(cancel_run))
        .route("/v1/runs/{run_id}/events", get(stream_events))
        .route(
            "/v1/conversations/{conversation_id}/messages",
            get(messages),
        )
        .route(
            "/v1/conversations/{conversation_id}/variables",
            get(list_variables),
        )
        .route(
            "/v1/conversations/{conversation_id}/variables/{name}",
            axum::routing::put(put_variable).delete(delete_variable),
        )
        .with_state(pool);
    let listener = tokio::net::TcpListener::bind(config.api_bind).await?;
    tracing::info!(address = %config.api_bind, "API listening");
    axum::serve(listener, app).await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        flow_uses_rag, format_ops_metrics, idempotency_key, quota_exceeded, token_budget_exceeded,
        OpsMetrics,
    };
    use axum::http::{HeaderMap, HeaderValue};

    #[test]
    fn metrics_render_with_tenant_labels_and_usage() {
        let body = format_ops_metrics(
            uuid::Uuid::nil(),
            &OpsMetrics {
                queued_runs: 2,
                oldest_run_queue_seconds: 3.5,
                model_failures_5m: 4,
                provider_throttles_5m: 1,
                input_tokens_5m: 10,
                output_tokens_5m: 20,
            },
            Some(0.01),
        );
        assert!(body.contains(
            "chatflow_queued_runs{tenant_id=\"00000000-0000-0000-0000-000000000000\"} 2"
        ));
        assert!(body.contains("chatflow_model_estimated_cost_usd_5m"));
    }

    #[test]
    fn output_token_budget_reserves_declared_maxima() {
        let flow: chatflow::flow::FlowDefinition =
            serde_json::from_str(include_str!("../../examples/basic-chat.json")).unwrap();
        assert_eq!(
            chatflow::budget::reserve(&flow, None)
                .unwrap()
                .output_tokens,
            1024
        );
        assert!(!token_budget_exceeded(488, 512, 1000));
        assert!(token_budget_exceeded(489, 512, 1000));
    }

    #[test]
    fn detects_rag_flows_for_read_authorization() {
        let rag: serde_json::Value =
            serde_json::from_str(include_str!("../../examples/rag-chat.json")).unwrap();
        let chat: serde_json::Value =
            serde_json::from_str(include_str!("../../examples/basic-chat.json")).unwrap();
        assert!(flow_uses_rag(&rag));
        assert!(!flow_uses_rag(&chat));
    }

    #[test]
    fn run_quota_checks_both_active_and_rate_limits() {
        assert!(!quota_exceeded(1, 4, 2, 5));
        assert!(quota_exceeded(2, 0, 2, 5));
        assert!(quota_exceeded(0, 5, 2, 5));
    }

    #[test]
    fn idempotency_keys_are_bounded_and_header_safe() {
        let mut headers = HeaderMap::new();
        assert_eq!(idempotency_key(&headers).unwrap(), None);
        headers.insert(
            "idempotency-key",
            HeaderValue::from_static("request_1:turn.2"),
        );
        assert_eq!(
            idempotency_key(&headers).unwrap().as_deref(),
            Some("request_1:turn.2")
        );
        headers.insert("idempotency-key", HeaderValue::from_static("bad key"));
        assert!(idempotency_key(&headers).is_err());
        headers.insert(
            "idempotency-key",
            HeaderValue::from_str(&"a".repeat(129)).unwrap(),
        );
        assert!(idempotency_key(&headers).is_err());
    }
}
