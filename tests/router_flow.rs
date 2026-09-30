use std::{
    process::{Child, Command, Stdio},
    time::Duration,
};

use axum::{
    http::HeaderMap,
    routing::{get, post},
    Json, Router,
};
use chatflow::auth::generate_key;
use reqwest::{Client, StatusCode};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

struct Process(Child);

impl Drop for Process {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn mock_model(
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> ([(String, String); 1], String) {
    assert_eq!(
        headers.get("authorization").unwrap(),
        "Bearer model-test-key"
    );
    assert_eq!(headers.get("user-agent").unwrap(), "chatflow/0.1");
    assert_eq!(
        body["model"],
        "bedrock/eu.anthropic.claude-haiku-4-5-20251001-v1:0"
    );
    assert_eq!(body["stream_options"]["include_usage"], true);
    assert!(body["messages"].to_string().contains("cedar moon 417"));
    assert!(body["messages"]
        .to_string()
        .contains("What is the verification phrase?"));
    let chunks = [
        json!({"id":"chatcmpl-test","object":"chat.completion.chunk","created":1,"model":"bedrock/eu.anthropic.claude-haiku-4-5-20251001-v1:0","choices":[{"index":0,"delta":{"content":"The phrase is cedar moon 417."},"finish_reason":null}]}),
        json!({"id":"chatcmpl-test","object":"chat.completion.chunk","created":1,"model":"bedrock/eu.anthropic.claude-haiku-4-5-20251001-v1:0","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
        json!({"id":"chatcmpl-test","object":"chat.completion.chunk","created":1,"model":"bedrock/eu.anthropic.claude-haiku-4-5-20251001-v1:0","choices":[],"usage":{"prompt_tokens":20,"completion_tokens":8,"total_tokens":28}}),
    ];
    let mut sse = chunks
        .iter()
        .map(|chunk| format!("data: {chunk}\n\n"))
        .collect::<String>();
    sse.push_str("data: [DONE]\n\n");
    ([("content-type".into(), "text/event-stream".into())], sse)
}

async fn mock_ragflow(headers: HeaderMap, Json(body): Json<Value>) -> Json<Value> {
    assert_eq!(headers.get("authorization").unwrap(), "Bearer rag-test-key");
    assert_eq!(body["dataset_ids"], json!(["dataset_router_test"]));
    Json(json!({"code":0,"data":{"chunks":[{
        "id":"chunk_router_test",
        "dataset_id":"dataset_router_test",
        "document_id":"document_router_test",
        "content":"The project verification phrase is cedar moon 417."
    }]}}))
}

#[tokio::test]
async fn rag_run_uses_tenant_vault_key_and_router_stream() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let pool = PgPool::connect(&database_url).await.unwrap();
    sqlx::migrate!().run(&pool).await.unwrap();
    let tenant: Uuid = sqlx::query_scalar("INSERT INTO tenants(name) VALUES ($1) RETURNING id")
        .bind(Uuid::new_v4().to_string())
        .fetch_one(&pool)
        .await
        .unwrap();
    let (key, public_id, digest) = generate_key();
    sqlx::query(
        "INSERT INTO api_keys(tenant_id, public_id, secret_digest, scopes) VALUES ($1, $2, $3, $4)",
    )
    .bind(tenant)
    .bind(public_id)
    .bind(digest)
    .bind(vec![
        "credentials:write",
        "flows:write",
        "runs:write",
        "runs:read",
        "rag:read",
        "rag:configure",
    ])
    .execute(&pool)
    .await
    .unwrap();

    let model_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let model_url = format!("http://{}/openai", model_listener.local_addr().unwrap());
    let model_server = tokio::spawn(async move {
        axum::serve(
            model_listener,
            Router::new().route("/openai/chat/completions", post(mock_model)),
        )
        .await
        .unwrap();
    });
    let vault_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let vault_url = format!("http://{}", vault_listener.local_addr().unwrap());
    let vault_path = format!("/v1/secret/data/tenants/{tenant}/openai");
    let vault_server = tokio::spawn(async move {
        axum::serve(
            vault_listener,
            Router::new().route(
                &vault_path,
                get(|| async { Json(json!({"data":{"data":{"api_key":"model-test-key"}}})) }),
            ),
        )
        .await
        .unwrap();
    });
    let rag_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let rag_url = format!("http://{}", rag_listener.local_addr().unwrap());
    let rag_server = tokio::spawn(async move {
        axum::serve(
            rag_listener,
            Router::new().route("/api/v1/retrieval", post(mock_ragflow)),
        )
        .await
        .unwrap();
    });

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let base = format!("http://127.0.0.1:{port}");
    let api = Process(
        Command::new(env!("CARGO_BIN_EXE_api"))
            .env("DATABASE_URL", &database_url)
            .env("API_BIND", format!("127.0.0.1:{port}"))
            .env("MODEL_INPUT_USD_PER_MILLION", "1")
            .env("MODEL_OUTPUT_USD_PER_MILLION", "1")
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let client = Client::new();
    let mut ready = false;
    for _ in 0..100 {
        if client
            .get(format!("{base}/health/ready"))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
        {
            ready = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(ready, "API did not become ready");
    let credential_response = client
        .post(format!("{base}/v1/provider-credentials"))
        .bearer_auth(&key)
        .json(&json!({"provider_alias":"openai","secret_reference":format!("tenants/{tenant}/openai")}))
        .send()
        .await
        .unwrap();
    assert_eq!(credential_response.status(), StatusCode::CREATED);
    let credential: Value = credential_response.json().await.unwrap();
    let configured = client
        .put(format!("{base}/v1/ragflow-datasets"))
        .bearer_auth(&key)
        .json(&json!({"dataset_ids":["dataset_router_test"]}))
        .send()
        .await
        .unwrap();
    assert_eq!(configured.status(), StatusCode::OK);

    let mut flow: Value = serde_json::from_str(include_str!("../examples/rag-chat.json")).unwrap();
    flow["flow_id"] = json!(Uuid::new_v4());
    flow["nodes"][3]["config"]["model_alias"] = json!("openai");
    flow["nodes"][3]["config"]["credential_id"] = credential["credential_id"].clone();
    let flow_id = flow["flow_id"].as_str().unwrap();
    let published = client
        .post(format!("{base}/v1/flows/{flow_id}/versions"))
        .bearer_auth(&key)
        .json(&json!({"name":"Router RAG test", "definition":flow}))
        .send()
        .await
        .unwrap();
    assert_eq!(published.status(), StatusCode::CREATED);
    let version: Value = published.json().await.unwrap();
    let created = client
        .post(format!("{base}/v1/runs"))
        .bearer_auth(&key)
        .json(&json!({"flow_version_id":version["version_id"],"input":{"message":"What is the verification phrase?"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let run: Value = created.json().await.unwrap();
    let run_id = run["run_id"].as_str().unwrap();

    let worker = Process(
        Command::new(env!("CARGO_BIN_EXE_worker"))
            .env("DATABASE_URL", &database_url)
            .env("OPENAI_BASE_URL", &model_url)
            .env(
                "OPENAI_MODEL",
                "bedrock/eu.anthropic.claude-haiku-4-5-20251001-v1:0",
            )
            .env("VAULT_ADDR", &vault_url)
            .env("VAULT_TOKEN", "vault-test-token")
            .env("RAGFLOW_BASE_URL", &rag_url)
            .env("RAGFLOW_API_KEY", "rag-test-key")
            .env_remove("RAGFLOW_API_KEY_FILE")
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
    let mut result = None;
    for _ in 0..100 {
        let response: Value = client
            .get(format!("{base}/v1/runs/{run_id}"))
            .bearer_auth(&key)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if response["status"] == "succeeded" || response["status"] == "failed" {
            result = Some(response);
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let result = result.expect("router RAG run did not finish");
    assert_eq!(result["status"], "succeeded", "{result}");
    assert!(result["output"].to_string().contains("cedar moon 417"));
    let timeline: Value = client
        .get(format!("{base}/v1/runs/{run_id}/timeline"))
        .bearer_auth(&key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let events = timeline["events"].as_array().unwrap();
    assert!(events
        .iter()
        .any(|event| event["event_type"] == "rag.sources"
            && event["metadata"]["sources"][0]["document_id"] == "document_router_test"));
    assert!(events.iter().any(
        |event| event["event_type"] == "model.usage" && event["metadata"]["output_tokens"] == 8
    ));
    drop(worker);
    drop(api);
    model_server.abort();
    vault_server.abort();
    rag_server.abort();
}
