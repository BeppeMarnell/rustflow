use std::{
    process::{Child, Command, Stdio},
    time::Duration,
};

use chatflow::{auth::generate_key, flow::FlowDefinition};
use reqwest::{Client, StatusCode};
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use uuid::Uuid;

struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn spawn(binary: &str, database_url: &str, port: Option<u16>) -> Process {
    let mut command = Command::new(binary);
    command
        .env("DATABASE_URL", database_url)
        .env_remove("RAGFLOW_API_KEY_FILE")
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    if let Some(port) = port {
        command.env("API_BIND", format!("127.0.0.1:{port}"));
    }
    Process(command.spawn().expect("process starts"))
}

async fn mock_ragflow(
    headers: axum::http::HeaderMap,
    axum::Json(body): axum::Json<Value>,
) -> axum::Json<Value> {
    assert_eq!(
        headers.get("authorization").unwrap(),
        "Bearer test-ragflow-key"
    );
    assert_eq!(body["dataset_ids"], json!(["dataset_alpha"]));
    assert!(body["question"].as_str().unwrap().contains("ocean"));
    axum::Json(json!({"code":0,"data":{"chunks":[{
        "id":"chunk_ocean", "document_id":"document_ocean",
        "dataset_id":"dataset_alpha", "content":"The ocean is blue and deep."
    }]}}))
}

#[tokio::test]
async fn two_workers_complete_turns_and_revoked_key_is_rejected() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let pool = PgPool::connect(&database_url).await.unwrap();
    sqlx::migrate!().run(&pool).await.unwrap();
    let tenant_id: Uuid = sqlx::query_scalar("INSERT INTO tenants(name) VALUES ($1) RETURNING id")
        .bind(format!("test-{}", Uuid::new_v4()))
        .fetch_one(&pool)
        .await
        .unwrap();
    let rag_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let rag_url = format!("http://{}", rag_listener.local_addr().unwrap());
    let rag_server = tokio::spawn(async move {
        axum::serve(
            rag_listener,
            axum::Router::new().route("/api/v1/retrieval", axum::routing::post(mock_ragflow)),
        )
        .await
        .unwrap();
    });
    std::env::set_var("RAGFLOW_BASE_URL", rag_url);
    std::env::set_var("RAGFLOW_API_KEY", "test-ragflow-key");
    let (key, public_id, digest) = generate_key();
    sqlx::query(
        "INSERT INTO api_keys(tenant_id, public_id, secret_digest, scopes) VALUES ($1, $2, $3, $4)",
    )
    .bind(tenant_id)
    .bind(&public_id)
    .bind(digest)
    .bind(vec![
        "flows:read",
        "flows:write",
        "runs:write",
        "runs:read",
        "rag:read",
        "credentials:read",
        "credentials:write",
        "ops:read",
    ])
    .execute(&pool)
    .await
    .unwrap();
    let (rag_config_key, rag_config_public, rag_config_digest) = generate_key();
    sqlx::query(
        "INSERT INTO api_keys(tenant_id, public_id, secret_digest, scopes) VALUES ($1, $2, $3, $4)",
    )
    .bind(tenant_id)
    .bind(rag_config_public)
    .bind(rag_config_digest)
    .bind(vec!["rag:configure"])
    .execute(&pool)
    .await
    .unwrap();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let _api = spawn(env!("CARGO_BIN_EXE_api"), &database_url, Some(port));
    let client = Client::new();
    let base = format!("http://127.0.0.1:{port}");
    for _ in 0..100 {
        if client
            .get(format!("{base}/health/ready"))
            .send()
            .await
            .is_ok_and(|r| r.status().is_success())
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let forbidden = client
        .put(format!("{base}/v1/ragflow-datasets"))
        .bearer_auth(&key)
        .json(&json!({"dataset_ids":["dataset_alpha"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(forbidden.status(), StatusCode::FORBIDDEN);
    let configured = client
        .put(format!("{base}/v1/ragflow-datasets"))
        .bearer_auth(&rag_config_key)
        .json(&json!({"dataset_ids":["dataset_alpha"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(configured.status(), StatusCode::OK);
    let listed: Value = client
        .get(format!("{base}/v1/ragflow-datasets"))
        .bearer_auth(&rag_config_key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed["dataset_ids"], json!(["dataset_alpha"]));
    let invalid = client
        .put(format!("{base}/v1/ragflow-datasets"))
        .bearer_auth(&rag_config_key)
        .json(&json!({"dataset_ids":["other/invalid"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);

    let mut flow: FlowDefinition =
        serde_json::from_str(include_str!("../examples/basic-chat.json")).unwrap();
    flow.flow_id = Uuid::new_v4();
    let published = client
        .post(format!("{base}/v1/flows/{}/versions", flow.flow_id))
        .bearer_auth(&key)
        .json(&json!({"name":"Basic chat","definition":flow}))
        .send()
        .await
        .unwrap();
    assert_eq!(published.status(), StatusCode::CREATED);
    let version: Value = published.json().await.unwrap();
    let version_id = version["version_id"].as_str().unwrap();
    let listed: Value = client
        .get(format!("{base}/v1/flows"))
        .bearer_auth(&key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(listed["flows"]
        .as_array()
        .unwrap()
        .iter()
        .any(|entry| entry["flow_id"] == flow.flow_id.to_string()));
    let versions: Value = client
        .get(format!("{base}/v1/flows/{}/versions", flow.flow_id))
        .bearer_auth(&key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(versions["versions"][0]["version_id"], version["version_id"]);
    let fetched_version: Value = client
        .get(format!("{base}/v1/flows/{}/versions/1", flow.flow_id))
        .bearer_auth(&key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        fetched_version["definition"]["flow_id"],
        flow.flow_id.to_string()
    );
    let validated = client
        .post(format!("{base}/v1/flows/validate"))
        .bearer_auth(&key)
        .json(&flow)
        .send()
        .await
        .unwrap();
    assert_eq!(validated.status(), StatusCode::OK);
    let debug: Value = client
        .post(format!("{base}/v1/flows/dry-run"))
        .bearer_auth(&key)
        .json(&json!({"definition":flow,"input":{"message":"debug"}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(debug["nodes"].as_array().unwrap().len(), 4);
    assert!(debug["outputs"]["reply"]
        .as_str()
        .unwrap()
        .contains("debug"));
    let metrics = client
        .get(format!("{base}/v1/ops/metrics"))
        .bearer_auth(&key)
        .send()
        .await
        .unwrap();
    assert_eq!(metrics.status(), StatusCode::OK);
    assert!(metrics
        .text()
        .await
        .unwrap()
        .contains("chatflow_queued_runs"));

    let make_run = |message: &'static str| {
        client
            .post(format!("{base}/v1/runs"))
            .bearer_auth(&key)
            .json(&json!({"flow_version_id": version_id, "input":{"message":message}}))
            .send()
    };
    let cancelled: Value = make_run("cancel me").await.unwrap().json().await.unwrap();
    let cancelled_id = cancelled["run_id"].as_str().unwrap();
    let cancellation = client
        .post(format!("{base}/v1/runs/{cancelled_id}/cancel"))
        .bearer_auth(&key)
        .send()
        .await
        .unwrap();
    assert_eq!(cancellation.status(), StatusCode::OK);
    let cancelled_status: Value = client
        .get(format!("{base}/v1/runs/{cancelled_id}"))
        .bearer_auth(&key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(cancelled_status["status"], "cancelled");

    let request_key = format!("turn-{}", Uuid::new_v4());
    let idempotent_request = || {
        client
            .post(format!("{base}/v1/runs"))
            .bearer_auth(&key)
            .header("Idempotency-Key", &request_key)
            .json(&json!({"flow_version_id": version_id, "input":{"message":"once"}}))
            .send()
    };
    let original = idempotent_request().await.unwrap();
    assert_eq!(original.status(), StatusCode::CREATED);
    let original: Value = original.json().await.unwrap();
    let replay = idempotent_request().await.unwrap();
    assert_eq!(replay.status(), StatusCode::OK);
    let replay: Value = replay.json().await.unwrap();
    assert_eq!(original["run_id"], replay["run_id"]);
    let conflict = client
        .post(format!("{base}/v1/runs"))
        .bearer_auth(&key)
        .header("Idempotency-Key", &request_key)
        .json(&json!({"flow_version_id": version_id, "input":{"message":"different"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(conflict.status(), StatusCode::CONFLICT);

    let recovered: Value = make_run("recover me").await.unwrap().json().await.unwrap();
    let recovered_id = Uuid::parse_str(recovered["run_id"].as_str().unwrap()).unwrap();
    sqlx::query("UPDATE jobs SET status = 'leased', lease_owner = 'dead-worker', attempts = 1, lease_expires_at = now() - interval '1 second' WHERE tenant_id = $1 AND run_id = $2")
        .bind(tenant_id).bind(recovered_id).execute(&pool).await.unwrap();
    sqlx::query("INSERT INTO node_runs(tenant_id, run_id, node_id, status, attempt, output, finished_at) VALUES ($1, $2, 'message', 'succeeded', 1, $3, now())")
        .bind(tenant_id).bind(recovered_id).bind(json!({"value":"recover me"})).execute(&pool).await.unwrap();
    sqlx::query("UPDATE runs SET status = 'running' WHERE tenant_id = $1 AND id = $2")
        .bind(tenant_id)
        .bind(recovered_id)
        .execute(&pool)
        .await
        .unwrap();
    let exhausted: Value = make_run("exhaust me").await.unwrap().json().await.unwrap();
    let exhausted_id = Uuid::parse_str(exhausted["run_id"].as_str().unwrap()).unwrap();
    sqlx::query("UPDATE jobs SET status = 'leased', lease_owner = 'dead-worker-two', attempts = max_attempts, lease_expires_at = now() - interval '1 second' WHERE tenant_id = $1 AND run_id = $2")
        .bind(tenant_id).bind(exhausted_id).execute(&pool).await.unwrap();
    sqlx::query("UPDATE jobs SET lease_expires_at = now() - interval '1 second' WHERE tenant_id = $1 AND run_id = $2")
        .bind(tenant_id).bind(recovered_id).execute(&pool).await.unwrap();

    let mut worker_one = spawn(env!("CARGO_BIN_EXE_worker"), &database_url, None);
    let mut worker_two = spawn(env!("CARGO_BIN_EXE_worker"), &database_url, None);
    let (first, second) = tokio::join!(make_run("first"), make_run("second"));
    let first = first.unwrap();
    let second = second.unwrap();
    assert_eq!(first.status(), StatusCode::CREATED);
    assert_eq!(second.status(), StatusCode::CREATED);
    let first: Value = first.json().await.unwrap();
    let second: Value = second.json().await.unwrap();
    for run in [&first, &second, &recovered, &original] {
        let run_id = run["run_id"].as_str().unwrap();
        let mut completed = false;
        for _ in 0..100 {
            let status: Value = client
                .get(format!("{base}/v1/runs/{run_id}"))
                .bearer_auth(&key)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if status["status"] == "succeeded" {
                assert_eq!(status["flow_version_id"], version["version_id"]);
                completed = true;
                break;
            }
            assert_ne!(status["status"], "failed", "run failed: {status}");
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(completed, "run did not complete");
        let run_uuid = Uuid::parse_str(run_id).unwrap();
        let node_count: i64 = sqlx::query_scalar("SELECT count(*) FROM node_runs WHERE tenant_id = $1 AND run_id = $2 AND status = 'succeeded'")
            .bind(tenant_id).bind(run_uuid).fetch_one(&pool).await.unwrap();
        assert_eq!(node_count, 4);
        let timeline: Value = client
            .get(format!("{base}/v1/runs/{run_id}/timeline"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(timeline["nodes"].as_array().unwrap().len(), 4);
        assert!(timeline["events"]
            .as_array()
            .unwrap()
            .iter()
            .any(|event| event["event_type"] == "run.completed"));
        let stream = client
            .get(format!("{base}/v1/runs/{run_id}/events"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
        assert!(stream.contains("event: token"));
        assert!(stream.contains("event: run.completed"));
        let messages: Value = client
            .get(format!(
                "{base}/v1/conversations/{}/messages",
                run["conversation_id"].as_str().unwrap()
            ))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(messages["messages"].as_array().unwrap().len(), 2);
    }
    let mut rag_flow: FlowDefinition =
        serde_json::from_str(include_str!("../examples/rag-chat.json")).unwrap();
    rag_flow.flow_id = Uuid::new_v4();
    let rag_version: Value = client
        .post(format!("{base}/v1/flows/{}/versions", rag_flow.flow_id))
        .bearer_auth(&key)
        .json(&json!({"name":"RAG chat","definition":rag_flow}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let rag_run: Value = client.post(format!("{base}/v1/runs"))
        .bearer_auth(&key)
        .json(&json!({"flow_version_id": rag_version["version_id"], "input":{"message":"What color is the ocean?"}}))
        .send().await.unwrap().json().await.unwrap();
    let rag_run_id = rag_run["run_id"].as_str().unwrap();
    let mut rag_completed = false;
    for _ in 0..100 {
        let status: Value = client
            .get(format!("{base}/v1/runs/{rag_run_id}"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if status["status"] == "succeeded" {
            rag_completed = true;
            break;
        }
        assert_ne!(status["status"], "failed", "{status}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(rag_completed, "RAG run did not complete");
    let timeline: Value = client
        .get(format!("{base}/v1/runs/{rag_run_id}/timeline"))
        .bearer_auth(&key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(timeline["events"]
        .as_array()
        .unwrap()
        .iter()
        .any(|event| event["event_type"] == "rag.sources"
            && event["metadata"]["sources"][0]["document_id"] == "document_ocean"
            && event["metadata"]["sources"][0]["chunk_id"] == "chunk_ocean"));
    let (read_only_key, read_only_public, read_only_digest) = generate_key();
    sqlx::query(
        "INSERT INTO api_keys(tenant_id, public_id, secret_digest, scopes) VALUES ($1, $2, $3, $4)",
    )
    .bind(tenant_id)
    .bind(read_only_public)
    .bind(read_only_digest)
    .bind(vec!["runs:read"])
    .execute(&pool)
    .await
    .unwrap();
    for path in [
        format!("/v1/runs/{rag_run_id}"),
        format!("/v1/runs/{rag_run_id}/timeline"),
        format!("/v1/runs/{rag_run_id}/events"),
        format!(
            "/v1/conversations/{}/messages",
            rag_run["conversation_id"].as_str().unwrap()
        ),
    ] {
        let response = client
            .get(format!("{base}{path}"))
            .bearer_auth(&read_only_key)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
    }
    let ordinary_run = first["run_id"].as_str().unwrap();
    let ordinary_read = client
        .get(format!("{base}/v1/runs/{ordinary_run}"))
        .bearer_auth(&read_only_key)
        .send()
        .await
        .unwrap();
    assert_eq!(ordinary_read.status(), StatusCode::NOT_FOUND);

    sqlx::query("UPDATE api_keys SET scopes = array_remove(scopes, 'rag:read') WHERE tenant_id = $1 AND public_id = $2")
        .bind(tenant_id).bind(&public_id).execute(&pool).await.unwrap();
    let missing_rag_scope = client
        .get(format!("{base}/v1/runs/{rag_run_id}/timeline"))
        .bearer_auth(&key)
        .send()
        .await
        .unwrap();
    assert_eq!(missing_rag_scope.status(), StatusCode::FORBIDDEN);
    sqlx::query("UPDATE api_keys SET scopes = array_append(scopes, 'rag:read') WHERE tenant_id = $1 AND public_id = $2")
        .bind(tenant_id).bind(&public_id).execute(&pool).await.unwrap();

    let other_tenant: Uuid =
        sqlx::query_scalar("INSERT INTO tenants(name) VALUES ('other-rag-tenant') RETURNING id")
            .fetch_one(&pool)
            .await
            .unwrap();
    let (other_key, other_public, other_digest) = generate_key();
    sqlx::query(
        "INSERT INTO api_keys(tenant_id, public_id, secret_digest, scopes) VALUES ($1, $2, $3, $4)",
    )
    .bind(other_tenant)
    .bind(other_public)
    .bind(other_digest)
    .bind(vec!["rag:read", "runs:read"])
    .execute(&pool)
    .await
    .unwrap();
    let foreign_timeline = client
        .get(format!("{base}/v1/runs/{rag_run_id}/timeline"))
        .bearer_auth(&other_key)
        .send()
        .await
        .unwrap();
    assert_eq!(foreign_timeline.status(), StatusCode::NOT_FOUND);
    let other_datasets = client
        .get(format!("{base}/v1/ragflow-datasets"))
        .bearer_auth(&other_key)
        .send()
        .await
        .unwrap();
    assert_eq!(other_datasets.status(), StatusCode::FORBIDDEN);
    let configured_other = client
        .put(format!("{base}/v1/ragflow-datasets"))
        .bearer_auth(&other_key)
        .json(&json!({"dataset_ids":["dataset_alpha"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(configured_other.status(), StatusCode::FORBIDDEN);
    let original_datasets: Value = client
        .get(format!("{base}/v1/ragflow-datasets"))
        .bearer_auth(&rag_config_key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(original_datasets["dataset_ids"], json!(["dataset_alpha"]));

    let event_sequences = sqlx::query(
        "SELECT sequence FROM run_events WHERE tenant_id = $1 AND run_id = $2 ORDER BY sequence",
    )
    .bind(tenant_id)
    .bind(Uuid::parse_str(first["run_id"].as_str().unwrap()).unwrap())
    .fetch_all(&pool)
    .await
    .unwrap();
    for (index, row) in event_sequences.iter().enumerate() {
        assert_eq!(row.get::<i64, _>("sequence"), index as i64 + 1);
    }
    let cancelled_nodes: i64 =
        sqlx::query_scalar("SELECT count(*) FROM node_runs WHERE tenant_id = $1 AND run_id = $2")
            .bind(tenant_id)
            .bind(Uuid::parse_str(cancelled_id).unwrap())
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(cancelled_nodes, 0);
    let recovered_input_attempts: i64 = sqlx::query_scalar("SELECT count(*) FROM node_runs WHERE tenant_id = $1 AND run_id = $2 AND node_id = 'message'")
        .bind(tenant_id).bind(recovered_id).fetch_one(&pool).await.unwrap();
    assert_eq!(recovered_input_attempts, 1);
    let mut exhausted_failed = false;
    for _ in 0..100 {
        let status: Value = client
            .get(format!("{base}/v1/runs/{exhausted_id}"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if status["status"] == "failed" {
            exhausted_failed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(exhausted_failed, "exhausted lease was not reaped");

    let mut conditional: FlowDefinition =
        serde_json::from_str(include_str!("../examples/conditional-chat.json")).unwrap();
    conditional.flow_id = Uuid::new_v4();
    let conditional_version: Value = client
        .post(format!("{base}/v1/flows/{}/versions", conditional.flow_id))
        .bearer_auth(&key)
        .json(&json!({"name":"Conditional chat","definition":conditional}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    for (message, expected, skipped) in [
        ("hello", "Hello! hello", "fallback"),
        ("other", "You said: other", "greeting"),
    ] {
        let queued: Value = client.post(format!("{base}/v1/runs"))
            .bearer_auth(&key)
            .json(&json!({"flow_version_id": conditional_version["version_id"], "input":{"message":message}}))
            .send().await.unwrap().json().await.unwrap();
        let conditional_run_id = Uuid::parse_str(queued["run_id"].as_str().unwrap()).unwrap();
        let mut completed = false;
        for _ in 0..100 {
            let status: Value = client
                .get(format!("{base}/v1/runs/{conditional_run_id}"))
                .bearer_auth(&key)
                .send()
                .await
                .unwrap()
                .json()
                .await
                .unwrap();
            if status["status"] == "succeeded" {
                assert_eq!(status["output"]["reply"], expected);
                completed = true;
                break;
            }
            assert_ne!(
                status["status"], "failed",
                "conditional run failed: {status}"
            );
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        assert!(completed, "conditional run did not complete");
        let skipped_count: i64 = sqlx::query_scalar("SELECT count(*) FROM node_runs WHERE tenant_id = $1 AND run_id = $2 AND node_id = $3 AND status = 'skipped'")
            .bind(tenant_id).bind(conditional_run_id).bind(skipped).fetch_one(&pool).await.unwrap();
        assert_eq!(skipped_count, 1);
    }

    let mut parallel: FlowDefinition =
        serde_json::from_str(include_str!("../examples/conditional-chat.json")).unwrap();
    parallel.flow_id = Uuid::new_v4();
    for node in &mut parallel.nodes {
        node.run_if = None;
    }
    let parallel_version: Value = client
        .post(format!("{base}/v1/flows/{}/versions", parallel.flow_id))
        .bearer_auth(&key)
        .json(&json!({"name":"Parallel fan out","definition":parallel}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let parallel_run: Value = client
        .post(format!("{base}/v1/runs"))
        .bearer_auth(&key)
        .json(
            &json!({"flow_version_id":parallel_version["version_id"],"input":{"message":"hello"}}),
        )
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let parallel_id = Uuid::parse_str(parallel_run["run_id"].as_str().unwrap()).unwrap();
    let mut parallel_completed = false;
    for _ in 0..100 {
        let status: Value = client
            .get(format!("{base}/v1/runs/{parallel_id}"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if status["status"] == "succeeded" {
            assert_eq!(status["output"]["reply"], "Hello! hello");
            parallel_completed = true;
            break;
        }
        assert_ne!(status["status"], "failed", "parallel run failed: {status}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(parallel_completed, "parallel run did not complete");
    let branch_count: i64 = sqlx::query_scalar("SELECT count(*) FROM node_runs WHERE tenant_id = $1 AND run_id = $2 AND node_id IN ('greeting', 'fallback') AND status = 'succeeded'")
        .bind(tenant_id).bind(parallel_id).fetch_one(&pool).await.unwrap();
    assert_eq!(branch_count, 2);

    worker_one.0.kill().unwrap();
    worker_one.0.wait().unwrap();
    worker_two.0.kill().unwrap();
    worker_two.0.wait().unwrap();
    let long_message = "ocean ".repeat(200);
    let crash_run: Value = client
        .post(format!("{base}/v1/runs"))
        .bearer_auth(&key)
        .json(&json!({"flow_version_id": version_id, "input":{"message":long_message}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let crash_run_id = Uuid::parse_str(crash_run["run_id"].as_str().unwrap()).unwrap();
    let mut crash_worker = spawn(env!("CARGO_BIN_EXE_worker"), &database_url, None);
    let mut model_started = false;
    for _ in 0..200 {
        let running: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM node_runs WHERE tenant_id = $1 AND run_id = $2 AND node_id = 'model' AND status = 'running')")
            .bind(tenant_id).bind(crash_run_id).fetch_one(&pool).await.unwrap();
        if running {
            model_started = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(model_started, "model did not start before crash");
    crash_worker.0.kill().unwrap();
    crash_worker.0.wait().unwrap();
    sqlx::query("UPDATE jobs SET lease_expires_at = now() - interval '1 second' WHERE tenant_id = $1 AND run_id = $2")
        .bind(tenant_id).bind(crash_run_id).execute(&pool).await.unwrap();
    let _recovery_worker = spawn(env!("CARGO_BIN_EXE_worker"), &database_url, None);
    let mut recovered_after_crash = false;
    for _ in 0..600 {
        let status: String =
            sqlx::query_scalar("SELECT status FROM runs WHERE tenant_id = $1 AND id = $2")
                .bind(tenant_id)
                .bind(crash_run_id)
                .fetch_one(&pool)
                .await
                .unwrap();
        if status == "succeeded" {
            recovered_after_crash = true;
            break;
        }
        assert_ne!(status, "failed", "crashed run failed");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(recovered_after_crash, "crashed run was not recovered");
    let model_attempts: Vec<(i32, String)> = sqlx::query_as("SELECT attempt, status FROM node_runs WHERE tenant_id = $1 AND run_id = $2 AND node_id = 'model' ORDER BY attempt")
        .bind(tenant_id).bind(crash_run_id).fetch_all(&pool).await.unwrap();
    assert_eq!(
        model_attempts,
        vec![(1, "failed".into()), (2, "succeeded".into())]
    );

    sqlx::query("UPDATE api_keys SET max_runs_per_minute = 1 WHERE public_id = $1")
        .bind(&public_id)
        .execute(&pool)
        .await
        .unwrap();
    let throttled = make_run("too many").await.unwrap();
    assert_eq!(throttled.status(), StatusCode::TOO_MANY_REQUESTS);
    sqlx::query("UPDATE api_keys SET max_runs_per_minute = 60 WHERE public_id = $1")
        .bind(&public_id)
        .execute(&pool)
        .await
        .unwrap();

    sqlx::query("UPDATE api_keys SET max_output_tokens_per_day = 1 WHERE public_id = $1")
        .bind(&public_id)
        .execute(&pool)
        .await
        .unwrap();
    let over_budget = make_run("over token budget").await.unwrap();
    assert_eq!(over_budget.status(), StatusCode::TOO_MANY_REQUESTS);
    sqlx::query("UPDATE api_keys SET max_output_tokens_per_day = 1000000 WHERE public_id = $1")
        .bind(&public_id)
        .execute(&pool)
        .await
        .unwrap();

    sqlx::query("UPDATE api_keys SET max_input_tokens_per_day = 1 WHERE public_id = $1")
        .bind(&public_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        make_run("over input budget").await.unwrap().status(),
        StatusCode::TOO_MANY_REQUESTS
    );
    sqlx::query("UPDATE api_keys SET max_input_tokens_per_day = 20000000 WHERE public_id = $1")
        .bind(&public_id)
        .execute(&pool)
        .await
        .unwrap();

    let first_id = Uuid::parse_str(first["run_id"].as_str().unwrap()).unwrap();
    let input_used: i64 = sqlx::query_scalar("SELECT COALESCE(SUM(reserved_input_tokens), 0)::bigint FROM runs WHERE api_key_id = (SELECT id FROM api_keys WHERE public_id = $1) AND created_at > now() - interval '1 day'")
        .bind(&public_id).fetch_one(&pool).await.unwrap();
    let one_reservation: i64 = sqlx::query_scalar(
        "SELECT reserved_input_tokens FROM runs WHERE tenant_id = $1 AND id = $2",
    )
    .bind(tenant_id)
    .bind(first_id)
    .fetch_one(&pool)
    .await
    .unwrap();
    sqlx::query("UPDATE api_keys SET max_input_tokens_per_day = $1 WHERE public_id = $2")
        .bind(input_used + one_reservation)
        .bind(&public_id)
        .execute(&pool)
        .await
        .unwrap();
    let (race_one, race_two) =
        tokio::join!(make_run("budget race one"), make_run("budget race two"));
    let statuses = [race_one.unwrap().status(), race_two.unwrap().status()];
    assert!(statuses.contains(&StatusCode::CREATED));
    assert!(statuses.contains(&StatusCode::TOO_MANY_REQUESTS));
    sqlx::query("UPDATE api_keys SET max_input_tokens_per_day = 20000000 WHERE public_id = $1")
        .bind(&public_id)
        .execute(&pool)
        .await
        .unwrap();

    sqlx::query("UPDATE runs SET reserved_spend_microusd = 10 WHERE tenant_id = $1 AND id = $2")
        .bind(tenant_id)
        .bind(first_id)
        .execute(&pool)
        .await
        .unwrap();
    sqlx::query("UPDATE api_keys SET max_spend_microusd_per_day = 5 WHERE public_id = $1")
        .bind(&public_id)
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(
        make_run("over spend budget").await.unwrap().status(),
        StatusCode::TOO_MANY_REQUESTS
    );

    sqlx::query("UPDATE api_keys SET revoked_at = now() WHERE public_id = $1")
        .bind(public_id)
        .execute(&pool)
        .await
        .unwrap();
    let rejected = make_run("blocked").await.unwrap();
    assert_eq!(rejected.status(), StatusCode::UNAUTHORIZED);
    rag_server.abort();
    std::env::remove_var("RAGFLOW_BASE_URL");
    std::env::remove_var("RAGFLOW_API_KEY");
}
