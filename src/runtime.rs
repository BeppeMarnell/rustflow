use std::{collections::HashMap, sync::Arc, time::Duration};

use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

use crate::{
    events,
    flow::{self, FlowDefinition, NodeDefinition},
    node::{BooleanValue, NodeContext, NodeError, NodeFuture, NodeHandler, TextValue},
    provider::{
        ChatMessage, ChatRequest, ChatRole, MockProvider, ModelProvider, ProviderError, SinkFuture,
        TokenSink,
    },
    ragflow,
};

#[derive(Clone, Debug)]
pub struct ClaimedJob {
    pub tenant_id: Uuid,
    pub run_id: Uuid,
}

pub async fn claim(pool: &PgPool, owner: &str) -> Result<Option<ClaimedJob>> {
    let row = sqlx::query("UPDATE jobs SET status = 'leased', lease_owner = $1, lease_expires_at = now() + interval '20 seconds', heartbeat_at = now(), attempts = attempts + 1 FROM (SELECT id FROM jobs WHERE queue = 'interactive' AND attempts < max_attempts AND (status = 'queued' OR (status = 'leased' AND lease_expires_at < now())) AND available_at <= now() ORDER BY available_at, created_at LIMIT 1 FOR UPDATE SKIP LOCKED) candidate WHERE jobs.id = candidate.id RETURNING jobs.tenant_id, jobs.run_id")
        .bind(owner).fetch_optional(pool).await?;
    Ok(row.map(|r| ClaimedJob {
        tenant_id: r.get("tenant_id"),
        run_id: r.get("run_id"),
    }))
}

pub async fn heartbeat(pool: &PgPool, job: &ClaimedJob, owner: &str) -> Result<bool> {
    let result = sqlx::query("UPDATE jobs SET heartbeat_at = now(), lease_expires_at = now() + interval '20 seconds' WHERE tenant_id = $1 AND run_id = $2 AND status = 'leased' AND lease_owner = $3")
        .bind(job.tenant_id).bind(job.run_id).bind(owner).execute(pool).await?;
    Ok(result.rows_affected() == 1)
}

/// Marks one lease that exhausted its claim attempts as failed.
pub async fn reap_exhausted(pool: &PgPool) -> Result<bool> {
    let mut tx = pool.begin().await?;
    let row = sqlx::query("SELECT tenant_id, run_id FROM jobs WHERE status = 'leased' AND lease_expires_at < now() AND attempts >= max_attempts LIMIT 1 FOR UPDATE SKIP LOCKED")
        .fetch_optional(&mut *tx).await?;
    let Some(row) = row else { return Ok(false) };
    let tenant_id: Uuid = row.try_get("tenant_id")?;
    let run_id: Uuid = row.try_get("run_id")?;
    let message = "worker lease expired after maximum claim attempts";
    sqlx::query("UPDATE runs SET status = 'failed', error = $3, finished_at = now() WHERE tenant_id = $1 AND id = $2 AND status IN ('queued', 'running')")
        .bind(tenant_id).bind(run_id).bind(json!({"message": message})).execute(&mut *tx).await?;
    events::append(
        &mut tx,
        tenant_id,
        run_id,
        None,
        "run.failed",
        Some("failed"),
        json!({"message": message}),
    )
    .await?;
    sqlx::query("UPDATE jobs SET status = 'failed', lease_owner = NULL, lease_expires_at = NULL WHERE tenant_id = $1 AND run_id = $2")
        .bind(tenant_id).bind(run_id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(true)
}

fn may_replay(kind: &str, previous_attempt: i32, allowed_retries: u8) -> bool {
    kind != "llm.chat" || previous_attempt <= i32::from(allowed_retries)
}

fn should_retry(kind: &str, attempt: i32, allowed_retries: u8, retryable: bool) -> bool {
    kind == "llm.chat" && retryable && attempt <= i32::from(allowed_retries)
}

fn effective_timeout(
    node_timeout_ms: u64,
    deadline: chrono::DateTime<chrono::Utc>,
    now: chrono::DateTime<chrono::Utc>,
) -> Option<Duration> {
    let remaining = (deadline - now).to_std().ok()?;
    if remaining.is_zero() {
        return None;
    }
    Some(Duration::from_millis(node_timeout_ms).min(remaining))
}

fn is_provider_throttled(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("429") || lower.contains("rate limit") || lower.contains("too many requests")
}

fn retry_delay(attempt: i32) -> Duration {
    Duration::from_millis(100 * 2_u64.pow(attempt.saturating_sub(1).min(5) as u32))
}

fn ready_nodes<'a>(
    flow: &'a FlowDefinition,
    values: &HashMap<String, Value>,
) -> Vec<&'a NodeDefinition> {
    flow.nodes
        .iter()
        .filter(|node| {
            !values.contains_key(&node.id)
                && node
                    .bindings
                    .values()
                    .all(|binding| values.contains_key(&binding.node))
                && node
                    .run_if
                    .as_ref()
                    .is_none_or(|guard| values.contains_key(&guard.node))
        })
        .collect()
}

struct NoopSink;
impl TokenSink for NoopSink {
    fn emit<'a>(&'a self, _text: String) -> SinkFuture<'a> {
        Box::pin(async { Ok(()) })
    }
}

pub async fn dry_run(
    pool: &PgPool,
    tenant_id: Uuid,
    flow: &FlowDefinition,
    input: &Value,
) -> Result<Value> {
    flow::validate(flow)?;
    if flow
        .nodes
        .iter()
        .any(|node| node.kind.starts_with("variable."))
    {
        return Err(anyhow!("conversation variables require a persisted run"));
    }
    let provider = MockProvider;
    let sink = NoopSink;
    let context = NodeContext {
        tenant_id,
        run_id: Uuid::new_v4(),
        trace_id: String::new(),
    };
    let deadline =
        chrono::Utc::now() + chrono::Duration::milliseconds(flow.limits.max_run_ms as i64);
    let mut values = HashMap::<String, Value>::new();
    let mut trace = Vec::new();
    while values.len() < flow.nodes.len() {
        let ready = ready_nodes(flow, &values);
        if ready.is_empty() {
            return Err(anyhow!("flow has no runnable node"));
        }
        for node in ready {
            if let Some(guard) = &node.run_if {
                let condition = values
                    .get(&guard.node)
                    .and_then(|v| v.get("value"))
                    .and_then(Value::as_bool)
                    .context("guard did not return a boolean")?;
                if condition != guard.equals {
                    values.insert(node.id.clone(), Value::Null);
                    trace.push(json!({"node_id": node.id, "status": "skipped"}));
                    continue;
                }
            }
            let mut bound = serde_json::Map::new();
            for (port, source) in &node.bindings {
                let source_value = values.get(&source.node).context("missing bound source")?;
                let value = if source_value.is_null() {
                    Value::Null
                } else {
                    source_value
                        .get(&source.output)
                        .cloned()
                        .context("missing bound value")?
                };
                bound.insert(port.clone(), value);
            }
            let timeout = effective_timeout(node.timeout_ms, deadline, chrono::Utc::now())
                .context("dry run time limit exceeded")?;
            let output = tokio::time::timeout(timeout, async {
                if node.kind == "rag.retrieve" {
                    let query = bound
                        .get("query")
                        .and_then(Value::as_str)
                        .context("rag.retrieve requires a query string")?;
                    let top_k = node
                        .config
                        .get("top_k")
                        .and_then(Value::as_u64)
                        .context("rag.retrieve requires top_k")?
                        as usize;
                    let sources = ragflow::RagflowClient::from_env(pool.clone())?
                        .retrieve(tenant_id, query, top_k)
                        .await?;
                    return Ok::<Value, anyhow::Error>(rag_output(query, &sources));
                }
                let mut config = node.config.clone();
                if node.kind == "llm.chat" {
                    config["model_alias"] = json!("mock");
                }
                let handler = BuiltinHandler {
                    kind: match node.kind.as_str() {
                        "input" => "input",
                        "prompt.render" => "prompt.render",
                        "llm.chat" => "llm.chat",
                        "condition" => "condition",
                        "select" => "select",
                        "output" => "output",
                        _ => return Err(anyhow!("unknown node type")),
                    },
                    provider: &provider,
                    sink: &sink,
                    flow_input: input,
                };
                Ok(handler
                    .execute(&context, config, Value::Object(bound))
                    .await?)
            })
            .await
            .context("dry run node timed out")??;
            trace.push(json!({"node_id": node.id, "status": "succeeded", "output": output}));
            values.insert(node.id.clone(), output);
        }
    }
    let output = flow
        .outputs
        .iter()
        .map(|(name, binding)| {
            let value = values
                .get(&binding.node)
                .and_then(|v| v.get(&binding.output))
                .cloned()
                .context("missing flow output")?;
            Ok((name.clone(), value))
        })
        .collect::<Result<serde_json::Map<String, Value>>>()?;
    Ok(json!({"outputs": output, "nodes": trace, "simulated_models": true}))
}

fn rag_output(query: &str, sources: &[ragflow::Source]) -> Value {
    let context = sources
        .iter()
        .map(|source| {
            format!(
                "[dataset:{} document:{} chunk:{}]\n{}",
                source.dataset_id, source.document_id, source.chunk_id, source.content
            )
        })
        .collect::<Vec<_>>()
        .join("\n\n");
    let citations = sources.iter().map(|source| json!({
        "dataset_id": source.dataset_id, "document_id": source.document_id, "chunk_id": source.chunk_id
    })).collect::<Vec<_>>();
    json!({"value": format!("Question: {query}\n\nSources:\n{context}"), "context": context, "sources": citations})
}

#[tracing::instrument(skip(pool, provider, job), fields(run_id = %job.run_id, tenant_id = %job.tenant_id, worker = %owner))]
pub async fn execute(
    pool: &PgPool,
    provider: Arc<dyn ModelProvider>,
    job: ClaimedJob,
    owner: &str,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    require_lease(&mut tx, &job, owner).await?;
    let row = sqlx::query("SELECT r.input, v.definition, r.conversation_id FROM runs r JOIN flow_versions v ON v.id = r.flow_version_id AND v.tenant_id = r.tenant_id WHERE r.tenant_id = $1 AND r.id = $2")
        .bind(job.tenant_id).bind(job.run_id).fetch_one(&mut *tx).await?;
    let input: Value = row.try_get("input")?;
    let definition: Value = row.try_get("definition")?;
    let conversation_id: Uuid = row.try_get("conversation_id")?;
    let flow: FlowDefinition = serde_json::from_value(definition)?;
    flow::validate(&flow)?;
    let started_at: chrono::DateTime<chrono::Utc> = sqlx::query_scalar("UPDATE runs SET status = 'running', started_at = COALESCE(started_at, now()) WHERE tenant_id = $1 AND id = $2 AND status IN ('queued', 'running') RETURNING started_at")
        .bind(job.tenant_id).bind(job.run_id).fetch_one(&mut *tx).await?;
    let deadline = started_at + chrono::Duration::milliseconds(flow.limits.max_run_ms as i64);
    events::append(
        &mut tx,
        job.tenant_id,
        job.run_id,
        None,
        "run.started",
        Some("running"),
        json!({}),
    )
    .await?;
    tx.commit().await?;

    let mut values = HashMap::<String, Value>::new();
    let completed = sqlx::query("SELECT node_id, output FROM node_runs WHERE tenant_id = $1 AND run_id = $2 AND status IN ('succeeded', 'skipped') ORDER BY attempt")
        .bind(job.tenant_id).bind(job.run_id).fetch_all(pool).await?;
    for row in completed {
        values.insert(row.try_get("node_id")?, row.try_get("output")?);
    }
    let interrupted = sqlx::query("SELECT node_id, attempt FROM node_runs WHERE tenant_id = $1 AND run_id = $2 AND status = 'running' ORDER BY attempt")
        .bind(job.tenant_id).bind(job.run_id).fetch_all(pool).await?;
    for row in interrupted {
        let node_id: String = row.try_get("node_id")?;
        let attempt: i32 = row.try_get("attempt")?;
        let node = flow
            .nodes
            .iter()
            .find(|node| node.id == node_id)
            .context("checkpoint refers to unknown node")?;
        if !may_replay(&node.kind, attempt, node.retry.max_attempts) {
            let message = "interrupted model call requires an explicit retry policy";
            fail_run(pool, &job, owner, node, attempt, message).await?;
            return Err(anyhow!(message));
        }
        mark_interrupted(pool, &job, owner, node, attempt).await?;
    }

    while values.len() < flow.nodes.len() {
        let ready = ready_nodes(&flow, &values);
        if ready.is_empty() {
            return Err(anyhow!("flow has no runnable node"));
        }
        let mut active = Vec::new();
        for node in ready.into_iter().take(flow.limits.max_parallel_nodes) {
            if let Some(guard) = &node.run_if {
                let condition = values
                    .get(&guard.node)
                    .and_then(|value| value.get("value"))
                    .and_then(Value::as_bool)
                    .context("guard did not return a boolean")?;
                if condition != guard.equals {
                    skip_node(pool, &job, owner, node).await?;
                    values.insert(node.id.clone(), Value::Null);
                    continue;
                }
            }
            active.push(node);
        }
        if active.is_empty() {
            continue;
        }
        let outcomes = futures_util::future::join_all(active.iter().map(|node| async {
            let execution = NodeExecution {
                job: &job,
                owner,
                node,
                attempt: 0,
                flow_input: &input,
                values: &values,
                deadline,
            };
            (
                node.id.clone(),
                execute_one(pool, provider.as_ref(), execution).await,
            )
        }))
        .await;
        let mut first_error = None;
        for (node_id, result) in outcomes {
            match result {
                Ok(output) => {
                    values.insert(node_id, output);
                }
                Err(error) if first_error.is_none() => first_error = Some(error),
                Err(error) => tracing::error!(%error, "parallel node failed"),
            }
        }
        if let Some(error) = first_error {
            cleanup_running_after_failure(pool, &job).await?;
            return Err(error);
        }
    }

    let output = flow
        .outputs
        .iter()
        .map(|(name, binding)| {
            let value = values
                .get(&binding.node)
                .and_then(|value| value.get(&binding.output))
                .cloned()
                .ok_or_else(|| anyhow!("missing flow output"))?;
            Ok((name.clone(), value))
        })
        .collect::<Result<serde_json::Map<String, Value>>>()?;
    let final_content = output.values().next().and_then(Value::as_str).unwrap_or("");
    let mut tx = pool.begin().await?;
    require_lease(&mut tx, &job, owner).await?;
    sqlx::query("INSERT INTO messages(tenant_id, conversation_id, role, content) VALUES ($1, $2, 'assistant', $3)")
        .bind(job.tenant_id).bind(conversation_id).bind(final_content).execute(&mut *tx).await?;
    sqlx::query("UPDATE runs SET status = 'succeeded', output = $3, finished_at = now() WHERE tenant_id = $1 AND id = $2")
        .bind(job.tenant_id).bind(job.run_id).bind(Value::Object(output)).execute(&mut *tx).await?;
    events::append(
        &mut tx,
        job.tenant_id,
        job.run_id,
        None,
        "run.completed",
        Some("succeeded"),
        json!({}),
    )
    .await?;
    sqlx::query("UPDATE jobs SET status = 'done', lease_owner = NULL, lease_expires_at = NULL WHERE tenant_id = $1 AND run_id = $2")
        .bind(job.tenant_id).bind(job.run_id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

async fn require_lease(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    job: &ClaimedJob,
    owner: &str,
) -> Result<()> {
    let active: Option<Uuid> = sqlx::query_scalar("SELECT id FROM jobs WHERE tenant_id = $1 AND run_id = $2 AND status = 'leased' AND lease_owner = $3 AND lease_expires_at > now() FOR UPDATE")
        .bind(job.tenant_id).bind(job.run_id).bind(owner).fetch_optional(&mut **tx).await?;
    if active.is_none() {
        return Err(anyhow!("job lease lost"));
    }
    Ok(())
}

async fn start_node(
    pool: &PgPool,
    job: &ClaimedJob,
    owner: &str,
    node: &NodeDefinition,
) -> Result<i32> {
    let mut tx = pool.begin().await?;
    require_lease(&mut tx, job, owner).await?;
    let attempt: i32 = sqlx::query_scalar("SELECT COALESCE(MAX(attempt), 0) + 1 FROM node_runs WHERE tenant_id = $1 AND run_id = $2 AND node_id = $3")
        .bind(job.tenant_id).bind(job.run_id).bind(&node.id).fetch_one(&mut *tx).await?;
    sqlx::query("INSERT INTO node_runs(tenant_id, run_id, node_id, status, attempt) VALUES ($1, $2, $3, 'running', $4)")
        .bind(job.tenant_id).bind(job.run_id).bind(&node.id).bind(attempt).execute(&mut *tx).await?;
    events::append(
        &mut tx,
        job.tenant_id,
        job.run_id,
        Some(&node.id),
        "node.started",
        Some("running"),
        json!({"attempt": attempt}),
    )
    .await?;
    tx.commit().await?;
    Ok(attempt)
}

async fn finish_node(
    pool: &PgPool,
    job: &ClaimedJob,
    owner: &str,
    node: &NodeDefinition,
    attempt: i32,
    output: &Value,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    require_lease(&mut tx, job, owner).await?;
    sqlx::query("UPDATE node_runs SET status = 'succeeded', output = $5, finished_at = now() WHERE tenant_id = $1 AND run_id = $2 AND node_id = $3 AND attempt = $4")
        .bind(job.tenant_id).bind(job.run_id).bind(&node.id).bind(attempt).bind(output).execute(&mut *tx).await?;
    events::append(
        &mut tx,
        job.tenant_id,
        job.run_id,
        Some(&node.id),
        "node.completed",
        Some("succeeded"),
        json!({"attempt": attempt}),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn mark_interrupted(
    pool: &PgPool,
    job: &ClaimedJob,
    owner: &str,
    node: &NodeDefinition,
    attempt: i32,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    require_lease(&mut tx, job, owner).await?;
    sqlx::query("UPDATE node_runs SET status = 'failed', error = $5, finished_at = now() WHERE tenant_id = $1 AND run_id = $2 AND node_id = $3 AND attempt = $4 AND status = 'running'")
        .bind(job.tenant_id).bind(job.run_id).bind(&node.id).bind(attempt)
        .bind(json!({"message": "worker lease expired; retrying node"})).execute(&mut *tx).await?;
    events::append(
        &mut tx,
        job.tenant_id,
        job.run_id,
        Some(&node.id),
        "node.interrupted",
        Some("failed"),
        json!({"attempt": attempt}),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn skip_node(
    pool: &PgPool,
    job: &ClaimedJob,
    owner: &str,
    node: &NodeDefinition,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    require_lease(&mut tx, job, owner).await?;
    sqlx::query("INSERT INTO node_runs(tenant_id, run_id, node_id, status, attempt, output, finished_at) VALUES ($1, $2, $3, 'skipped', 1, 'null'::jsonb, now())")
        .bind(job.tenant_id).bind(job.run_id).bind(&node.id).execute(&mut *tx).await?;
    events::append(
        &mut tx,
        job.tenant_id,
        job.run_id,
        Some(&node.id),
        "node.skipped",
        Some("skipped"),
        json!({}),
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn fail_node_attempt(
    pool: &PgPool,
    job: &ClaimedJob,
    owner: &str,
    node: &NodeDefinition,
    attempt: i32,
    message: &str,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    require_lease(&mut tx, job, owner).await?;
    sqlx::query("UPDATE node_runs SET status = 'failed', error = $5, finished_at = now() WHERE tenant_id = $1 AND run_id = $2 AND node_id = $3 AND attempt = $4")
        .bind(job.tenant_id).bind(job.run_id).bind(&node.id).bind(attempt).bind(json!({"message": message})).execute(&mut *tx).await?;
    events::append(
        &mut tx,
        job.tenant_id,
        job.run_id,
        Some(&node.id),
        "node.failed",
        Some("failed"),
        json!({"attempt": attempt, "message": message, "retrying": true}),
    )
    .await?;
    if node.kind == "llm.chat" {
        events::append(
            &mut tx,
            job.tenant_id,
            job.run_id,
            Some(&node.id),
            "model.failure",
            Some("failed"),
            json!({"attempt": attempt, "retrying": true, "throttled": is_provider_throttled(message)}),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

async fn fail_run(
    pool: &PgPool,
    job: &ClaimedJob,
    owner: &str,
    node: &NodeDefinition,
    attempt: i32,
    message: &str,
) -> Result<()> {
    let mut tx = pool.begin().await?;
    require_lease(&mut tx, job, owner).await?;
    sqlx::query("UPDATE node_runs SET status = 'failed', error = $5, finished_at = now() WHERE tenant_id = $1 AND run_id = $2 AND node_id = $3 AND attempt = $4")
        .bind(job.tenant_id).bind(job.run_id).bind(&node.id).bind(attempt).bind(json!({"message": message})).execute(&mut *tx).await?;
    events::append(
        &mut tx,
        job.tenant_id,
        job.run_id,
        Some(&node.id),
        "node.failed",
        Some("failed"),
        json!({"attempt": attempt, "message": message}),
    )
    .await?;
    if node.kind == "llm.chat" {
        events::append(
            &mut tx,
            job.tenant_id,
            job.run_id,
            Some(&node.id),
            "model.failure",
            Some("failed"),
            json!({"attempt": attempt, "retrying": false, "throttled": is_provider_throttled(message)}),
        )
        .await?;
    }
    sqlx::query("UPDATE runs SET status = 'failed', error = $3, finished_at = now() WHERE tenant_id = $1 AND id = $2")
        .bind(job.tenant_id).bind(job.run_id).bind(json!({"message": message})).execute(&mut *tx).await?;
    events::append(
        &mut tx,
        job.tenant_id,
        job.run_id,
        None,
        "run.failed",
        Some("failed"),
        json!({"message": message}),
    )
    .await?;
    sqlx::query("UPDATE jobs SET status = 'failed', lease_owner = NULL, lease_expires_at = NULL WHERE tenant_id = $1 AND run_id = $2")
        .bind(job.tenant_id).bind(job.run_id).execute(&mut *tx).await?;
    tx.commit().await?;
    Ok(())
}

async fn cleanup_running_after_failure(pool: &PgPool, job: &ClaimedJob) -> Result<()> {
    let mut tx = pool.begin().await?;
    let status: String = sqlx::query_scalar(
        "SELECT status FROM jobs WHERE tenant_id = $1 AND run_id = $2 FOR UPDATE",
    )
    .bind(job.tenant_id)
    .bind(job.run_id)
    .fetch_one(&mut *tx)
    .await?;
    if !matches!(status.as_str(), "failed" | "cancelled") {
        return Ok(());
    }
    let node_status = if status == "cancelled" {
        "skipped"
    } else {
        "failed"
    };
    let rows = sqlx::query("UPDATE node_runs SET status = $3, error = $4, finished_at = now() WHERE tenant_id = $1 AND run_id = $2 AND status = 'running' RETURNING node_id, attempt")
        .bind(job.tenant_id).bind(job.run_id).bind(node_status)
        .bind(json!({"message": "run ended while node was active"}))
        .fetch_all(&mut *tx).await?;
    for row in rows {
        let node_id: String = row.try_get("node_id")?;
        let attempt: i32 = row.try_get("attempt")?;
        events::append(
            &mut tx,
            job.tenant_id,
            job.run_id,
            Some(&node_id),
            "node.stopped",
            Some(node_status),
            json!({"attempt": attempt}),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

struct DatabaseTokenSink {
    pool: PgPool,
    job: ClaimedJob,
    owner: String,
    node_id: String,
    attempt: i32,
    model_alias: String,
}

impl TokenSink for DatabaseTokenSink {
    fn emit<'a>(&'a self, text: String) -> SinkFuture<'a> {
        Box::pin(async move {
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|e| ProviderError::Request(e.to_string()))?;
            require_lease(&mut tx, &self.job, &self.owner)
                .await
                .map_err(|e| ProviderError::Request(e.to_string()))?;
            events::append(
                &mut tx,
                self.job.tenant_id,
                self.job.run_id,
                Some(&self.node_id),
                "token",
                None,
                json!({"text": text, "attempt": self.attempt}),
            )
            .await
            .map_err(|e| ProviderError::Request(e.to_string()))?;
            tx.commit()
                .await
                .map_err(|e| ProviderError::Request(e.to_string()))?;
            Ok(())
        })
    }

    fn usage<'a>(
        &'a self,
        input_tokens: Option<u32>,
        output_tokens: Option<u32>,
    ) -> SinkFuture<'a> {
        Box::pin(async move {
            let mut tx = self
                .pool
                .begin()
                .await
                .map_err(|e| ProviderError::Request(e.to_string()))?;
            require_lease(&mut tx, &self.job, &self.owner)
                .await
                .map_err(|e| ProviderError::Request(e.to_string()))?;
            events::append(
                &mut tx,
                self.job.tenant_id,
                self.job.run_id,
                Some(&self.node_id),
                "model.usage",
                None,
                json!({"input_tokens": input_tokens, "output_tokens": output_tokens, "model_alias": self.model_alias}),
            )
            .await
            .map_err(|e| ProviderError::Request(e.to_string()))?;
            tx.commit()
                .await
                .map_err(|e| ProviderError::Request(e.to_string()))?;
            Ok(())
        })
    }
}

#[derive(Clone, Copy)]
struct NodeExecution<'a> {
    job: &'a ClaimedJob,
    owner: &'a str,
    node: &'a NodeDefinition,
    attempt: i32,
    flow_input: &'a Value,
    values: &'a HashMap<String, Value>,
    deadline: chrono::DateTime<chrono::Utc>,
}

async fn execute_one(
    pool: &PgPool,
    provider: &dyn ModelProvider,
    execution: NodeExecution<'_>,
) -> Result<Value> {
    let NodeExecution {
        job, owner, node, ..
    } = execution;
    loop {
        let attempt = start_node(pool, job, owner, node).await?;
        tracing::info!(run_id = %job.run_id, node_id = %node.id, attempt, "node started");
        let Some(timeout) =
            effective_timeout(node.timeout_ms, execution.deadline, chrono::Utc::now())
        else {
            fail_run(pool, job, owner, node, attempt, "run time limit exceeded").await?;
            return Err(anyhow!("run time limit exceeded"));
        };
        let result = tokio::time::timeout(
            timeout,
            run_node(
                pool,
                provider,
                NodeExecution {
                    attempt,
                    ..execution
                },
            ),
        )
        .await;
        match result {
            Ok(Ok(output)) => {
                finish_node(pool, job, owner, node, attempt, &output).await?;
                tracing::info!(run_id = %job.run_id, node_id = %node.id, attempt, "node completed");
                return Ok(output);
            }
            other => {
                let (error, retryable) = match other {
                    Ok(Err(error)) => {
                        let retryable = matches!(
                            error.downcast_ref::<NodeError>(),
                            Some(NodeError::Retryable(_))
                        );
                        (error, retryable)
                    }
                    Err(_) => (anyhow!("node timed out"), true),
                    Ok(Ok(_)) => unreachable!(),
                };
                if should_retry(&node.kind, attempt, node.retry.max_attempts, retryable) {
                    fail_node_attempt(pool, job, owner, node, attempt, &error.to_string()).await?;
                    tokio::time::sleep(retry_delay(attempt)).await;
                } else {
                    fail_run(pool, job, owner, node, attempt, &error.to_string()).await?;
                    return Err(error);
                }
            }
        }
    }
}

#[tracing::instrument(skip_all, fields(run_id = %execution.job.run_id, node_id = %execution.node.id, node_type = %execution.node.kind))]
async fn run_node(
    pool: &PgPool,
    provider: &dyn ModelProvider,
    execution: NodeExecution<'_>,
) -> Result<Value> {
    let NodeExecution {
        job,
        owner,
        node,
        attempt,
        flow_input,
        values,
        ..
    } = execution;
    let kind = match node.kind.as_str() {
        "input" => "input",
        "prompt.render" => "prompt.render",
        "llm.chat" => "llm.chat",
        "condition" => "condition",
        "select" => "select",
        "output" => "output",
        "rag.retrieve" => "rag.retrieve",
        "variable.get" => "variable.get",
        "variable.set" => "variable.set",
        _ => return Err(anyhow!("unknown node type")),
    };
    let mut bound = serde_json::Map::new();
    for (port, source) in &node.bindings {
        let source_value = values.get(&source.node).context("missing bound source")?;
        let value = if source_value.is_null() {
            Value::Null
        } else {
            source_value
                .get(&source.output)
                .cloned()
                .context("missing bound value")?
        };
        bound.insert(port.clone(), value);
    }
    if matches!(kind, "variable.get" | "variable.set") {
        let name = node
            .config
            .get("name")
            .and_then(Value::as_str)
            .context("variable name is missing")?;
        let conversation_id: Uuid =
            sqlx::query_scalar("SELECT conversation_id FROM runs WHERE tenant_id = $1 AND id = $2")
                .bind(job.tenant_id)
                .bind(job.run_id)
                .fetch_one(pool)
                .await?;
        if kind == "variable.get" {
            let value: Option<Value> = sqlx::query_scalar("SELECT value FROM conversation_variables WHERE tenant_id = $1 AND conversation_id = $2 AND name = $3")
                .bind(job.tenant_id).bind(conversation_id).bind(name).fetch_optional(pool).await?;
            let value = value.context("conversation variable is missing")?;
            let value = value
                .as_str()
                .context("conversation variable is not a string")?;
            return Ok(json!({"value": value}));
        }
        let value = bound
            .get("value")
            .and_then(Value::as_str)
            .context("variable.set requires a string")?;
        if serde_json::to_vec(&json!(value))?.len() > 16_384 {
            return Err(anyhow!("conversation variable exceeds 16384 bytes"));
        }
        let mut tx = pool.begin().await?;
        require_lease(&mut tx, job, owner).await?;
        sqlx::query("SELECT id FROM conversations WHERE tenant_id = $1 AND id = $2 FOR UPDATE")
            .bind(job.tenant_id)
            .bind(conversation_id)
            .fetch_one(&mut *tx)
            .await?;
        let count: i64 = sqlx::query_scalar("SELECT count(*) FROM conversation_variables WHERE tenant_id = $1 AND conversation_id = $2")
            .bind(job.tenant_id).bind(conversation_id).fetch_one(&mut *tx).await?;
        let exists: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM conversation_variables WHERE tenant_id = $1 AND conversation_id = $2 AND name = $3)")
            .bind(job.tenant_id).bind(conversation_id).bind(name).fetch_one(&mut *tx).await?;
        if !exists && count >= 64 {
            return Err(anyhow!("conversation variable limit reached"));
        }
        sqlx::query("INSERT INTO conversation_variables(tenant_id, conversation_id, name, value) VALUES ($1, $2, $3, $4) ON CONFLICT (tenant_id, conversation_id, name) DO UPDATE SET value = EXCLUDED.value, updated_at = now()")
            .bind(job.tenant_id).bind(conversation_id).bind(name).bind(json!(value))
            .execute(&mut *tx).await?;
        tx.commit().await?;
        return Ok(json!({"value": value}));
    }
    if kind == "rag.retrieve" {
        let query = bound
            .get("query")
            .and_then(Value::as_str)
            .context("rag.retrieve requires a query string")?;
        let top_k = node
            .config
            .get("top_k")
            .and_then(Value::as_u64)
            .context("rag.retrieve requires top_k")? as usize;
        let sources = ragflow::RagflowClient::from_env(pool.clone())?
            .retrieve(job.tenant_id, query, top_k)
            .await?;
        let output = rag_output(query, &sources);
        let citations = output["sources"].clone();
        let mut tx = pool.begin().await?;
        require_lease(&mut tx, job, owner).await?;
        events::append(
            &mut tx,
            job.tenant_id,
            job.run_id,
            Some(&node.id),
            "rag.sources",
            None,
            json!({"sources": citations}),
        )
        .await?;
        tx.commit().await?;
        let _: crate::node::RagValue = serde_json::from_value(output.clone())?;
        return Ok(output);
    }
    let sink = DatabaseTokenSink {
        pool: pool.clone(),
        job: job.clone(),
        owner: owner.to_owned(),
        node_id: node.id.clone(),
        attempt,
        model_alias: node
            .config
            .get("model_alias")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_owned(),
    };
    let handler = BuiltinHandler {
        kind,
        provider,
        sink: &sink,
        flow_input,
    };
    let context = NodeContext {
        tenant_id: job.tenant_id,
        run_id: job.run_id,
        trace_id: job.run_id.to_string(),
    };
    let output = handler
        .execute(&context, node.config.clone(), Value::Object(bound))
        .await?;
    if node.kind == "condition" {
        let _: BooleanValue = serde_json::from_value(output.clone())
            .context("node returned invalid condition output")?;
    } else {
        let _: TextValue =
            serde_json::from_value(output.clone()).context("node returned invalid output")?;
    }
    Ok(output)
}

fn render_prompt(template: &str, text: &str, question: Option<&str>) -> Result<String, NodeError> {
    let mut rendered = String::with_capacity(template.len() + text.len());
    let mut remaining = template;
    while let Some(start) = remaining.find("{{") {
        rendered.push_str(&remaining[..start]);
        remaining = &remaining[start..];
        if let Some(rest) = remaining.strip_prefix("{{text}}") {
            rendered.push_str(text);
            remaining = rest;
        } else if let Some(rest) = remaining.strip_prefix("{{question}}") {
            rendered.push_str(
                question.ok_or_else(|| NodeError::InvalidInput("missing question input".into()))?,
            );
            remaining = rest;
        } else {
            rendered.push_str("{{");
            remaining = &remaining[2..];
        }
    }
    rendered.push_str(remaining);
    Ok(rendered)
}

struct BuiltinHandler<'a> {
    kind: &'static str,
    provider: &'a dyn ModelProvider,
    sink: &'a dyn TokenSink,
    flow_input: &'a Value,
}

impl NodeHandler for BuiltinHandler<'_> {
    fn kind(&self) -> &'static str {
        self.kind
    }

    fn config_schema(&self) -> Value {
        flow::node_catalog_schema()[self.kind]["config"].clone()
    }

    fn input_schema(&self) -> Value {
        crate::node::interface_schema(self.kind)
            .expect("known node")
            .0
    }

    fn output_schema(&self) -> Value {
        crate::node::interface_schema(self.kind)
            .expect("known node")
            .1
    }

    fn execute<'a>(
        &'a self,
        context: &'a NodeContext,
        config: Value,
        input: Value,
    ) -> NodeFuture<'a> {
        Box::pin(async move {
            let field = |value: &Value, name: &str| -> Result<String, NodeError> {
                value
                    .get(name)
                    .and_then(Value::as_str)
                    .map(str::to_owned)
                    .ok_or_else(|| NodeError::InvalidInput(format!("missing string field: {name}")))
            };
            if self.kind == "condition" {
                return Ok(json!({"value": field(&input, "text")? == field(&config, "equals")?}));
            }
            if self.kind == "select" {
                let condition = input
                    .get("condition")
                    .and_then(Value::as_bool)
                    .ok_or_else(|| NodeError::InvalidInput("missing boolean condition".into()))?;
                let branch = if condition { "when_true" } else { "when_false" };
                return Ok(json!({"value": field(&input, branch)?}));
            }
            let value = match self.kind {
                "input" => field(self.flow_input, &field(&config, "name")?)?,
                "prompt.render" => render_prompt(
                    &field(&config, "template")?,
                    &field(&input, "text")?,
                    input.get("question").and_then(Value::as_str),
                )?,
                "llm.chat" => {
                    let max_output_tokens = config
                        .get("max_output_tokens")
                        .and_then(Value::as_u64)
                        .and_then(|n| u32::try_from(n).ok())
                        .ok_or_else(|| NodeError::InvalidInput("invalid token limit".into()))?;
                    let max_prompt_bytes = config
                        .get("max_prompt_bytes")
                        .and_then(Value::as_u64)
                        .unwrap_or(u64::from(crate::flow::default_max_prompt_bytes()));
                    let prompt = field(&input, "prompt")?;
                    if prompt.len() as u64 > max_prompt_bytes {
                        return Err(NodeError::InvalidInput(
                            "model prompt exceeds max_prompt_bytes".into(),
                        ));
                    }
                    self.provider
                        .chat(
                            ChatRequest {
                                tenant_id: context.tenant_id,
                                model_alias: field(&config, "model_alias")?,
                                credential_id: config
                                    .get("credential_id")
                                    .and_then(Value::as_str)
                                    .and_then(|id| Uuid::parse_str(id).ok()),
                                messages: vec![ChatMessage {
                                    role: ChatRole::User,
                                    content: prompt,
                                }],
                                max_output_tokens,
                            },
                            self.sink,
                        )
                        .await
                        .map_err(|error| match error {
                            ProviderError::UnknownModel(_) => {
                                NodeError::Execution(error.to_string())
                            }
                            ProviderError::Request(_) => NodeError::Retryable(error.to_string()),
                        })?
                        .content
                }
                "output" => field(&input, "value")?,
                _ => return Err(NodeError::InvalidInput("unknown node type".into())),
            };
            Ok(json!({"value": value}))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        effective_timeout, is_provider_throttled, may_replay, ready_nodes, render_prompt,
        retry_delay, should_retry, BuiltinHandler,
    };
    use crate::{
        node::{NodeContext, NodeHandler},
        provider::{MockProvider, SinkFuture, TokenSink},
    };
    use serde_json::{json, Value};
    use std::collections::HashMap;
    use uuid::Uuid;

    struct NoopSink;
    impl TokenSink for NoopSink {
        fn emit<'a>(&'a self, _text: String) -> SinkFuture<'a> {
            Box::pin(async { Ok(()) })
        }
    }

    #[test]
    fn prompt_render_keeps_source_and_question_values_literal() {
        let rendered = render_prompt(
            "Question: {{question}}\nSources: {{text}}",
            "source {{question}}",
            Some("ask {{text}}"),
        )
        .unwrap();
        assert_eq!(
            rendered,
            "Question: ask {{text}}\nSources: source {{question}}"
        );
        assert!(render_prompt("{{question}}", "source", None).is_err());
    }

    #[test]
    fn rag_output_keeps_legacy_value_and_exposes_source_only_context() {
        let source = crate::ragflow::Source {
            dataset_id: "dataset-a".into(),
            document_id: "document-b".into(),
            chunk_id: "chunk-c".into(),
            content: "cedar moon 417".into(),
        };
        let output = super::rag_output("Where is cedar moon?", &[source]);
        let context = output["context"].as_str().unwrap();
        assert_eq!(
            context,
            "[dataset:dataset-a document:document-b chunk:chunk-c]\ncedar moon 417"
        );
        assert_eq!(
            output["value"],
            format!("Question: Where is cedar moon?\n\nSources:\n{context}")
        );
        assert_eq!(output["sources"][0]["chunk_id"], "chunk-c");
    }

    #[test]
    fn interrupted_model_calls_need_explicit_retry_budget() {
        assert!(!may_replay("llm.chat", 1, 0));
        assert!(may_replay("llm.chat", 1, 1));
        assert!(!may_replay("llm.chat", 2, 1));
        assert!(may_replay("prompt.render", 10, 0));
    }

    #[test]
    fn identifies_provider_rate_limits() {
        assert!(is_provider_throttled("HTTP 429"));
        assert!(is_provider_throttled("Rate limit exceeded"));
        assert!(!is_provider_throttled("connection reset"));
    }

    #[test]
    fn run_deadline_caps_each_node_and_survives_resume() {
        let now = chrono::Utc::now();
        assert_eq!(
            effective_timeout(1_000, now + chrono::Duration::milliseconds(250), now)
                .unwrap()
                .as_millis(),
            250
        );
        assert_eq!(
            effective_timeout(100, now + chrono::Duration::seconds(1), now)
                .unwrap()
                .as_millis(),
            100
        );
        assert!(effective_timeout(100, now, now).is_none());
    }

    #[test]
    fn model_retries_are_bounded_and_only_for_retryable_failures() {
        assert!(should_retry("llm.chat", 1, 1, true));
        assert!(!should_retry("llm.chat", 2, 1, true));
        assert!(!should_retry("llm.chat", 1, 1, false));
        assert!(!should_retry("prompt.render", 1, 3, true));
        assert_eq!(retry_delay(1).as_millis(), 100);
        assert_eq!(retry_delay(20).as_millis(), 3_200);
    }

    #[tokio::test]
    async fn condition_selects_only_the_active_branch() {
        let provider = MockProvider;
        let sink = NoopSink;
        let flow_input = Value::Null;
        let context = NodeContext {
            tenant_id: Uuid::nil(),
            run_id: Uuid::nil(),
            trace_id: String::new(),
        };
        let condition = BuiltinHandler {
            kind: "condition",
            provider: &provider,
            sink: &sink,
            flow_input: &flow_input,
        };
        let result = condition
            .execute(&context, json!({"equals":"hello"}), json!({"text":"hello"}))
            .await
            .unwrap();
        assert_eq!(result, json!({"value":true}));
        let select = BuiltinHandler {
            kind: "select",
            provider: &provider,
            sink: &sink,
            flow_input: &flow_input,
        };
        let chosen = select
            .execute(
                &context,
                json!({}),
                json!({"condition":true,"when_true":"greeting","when_false":null}),
            )
            .await
            .unwrap();
        assert_eq!(chosen, json!({"value":"greeting"}));
        assert!(select
            .execute(
                &context,
                json!({}),
                json!({"condition":false,"when_true":"greeting","when_false":null})
            )
            .await
            .is_err());
    }

    #[tokio::test]
    async fn model_rejects_prompt_above_declared_byte_limit() {
        let provider = MockProvider;
        let sink = NoopSink;
        let flow_input = Value::Null;
        let context = NodeContext {
            tenant_id: Uuid::nil(),
            run_id: Uuid::nil(),
            trace_id: String::new(),
        };
        let model = BuiltinHandler {
            kind: "llm.chat",
            provider: &provider,
            sink: &sink,
            flow_input: &flow_input,
        };
        let result = model
            .execute(
                &context,
                json!({"model_alias":"mock","max_output_tokens":8,"max_prompt_bytes":4}),
                json!({"prompt":"hello"}),
            )
            .await;
        assert!(result.unwrap_err().to_string().contains("max_prompt_bytes"));
    }

    #[tokio::test]
    async fn dry_run_traces_nodes_and_simulates_model() {
        let flow: crate::flow::FlowDefinition =
            serde_json::from_str(include_str!("../examples/basic-chat.json")).unwrap();
        let pool = sqlx::postgres::PgPoolOptions::new()
            .connect_lazy("postgres://localhost/chatflow")
            .unwrap();
        let result = super::dry_run(&pool, Uuid::nil(), &flow, &json!({"message":"hello"}))
            .await
            .unwrap();
        assert_eq!(result["nodes"].as_array().unwrap().len(), 4);
        assert!(result["outputs"]["reply"]
            .as_str()
            .unwrap()
            .contains("hello"));
        assert_eq!(result["simulated_models"], true);
    }

    #[test]
    fn independent_nodes_become_ready_together() {
        let mut flow: crate::flow::FlowDefinition =
            serde_json::from_str(include_str!("../examples/conditional-chat.json")).unwrap();
        flow.nodes.iter_mut().for_each(|node| node.run_if = None);
        let mut values = HashMap::new();
        values.insert("message".to_owned(), json!({"value":"hello"}));
        let ready: Vec<_> = ready_nodes(&flow, &values)
            .into_iter()
            .map(|node| node.id.as_str())
            .collect();
        assert_eq!(ready, vec!["is_hello", "greeting", "fallback"]);
    }
}
