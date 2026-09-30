use std::{
    process::{Command, Stdio},
    time::Duration,
};

use chatflow::auth::generate_key;
use reqwest::{Client, StatusCode};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

#[tokio::test]
async fn credentials_are_tenant_scoped_and_required_for_openai_flows() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let pool = PgPool::connect(&database_url).await.unwrap();
    sqlx::migrate!().run(&pool).await.unwrap();
    let mut keys = Vec::new();
    let mut tenants = Vec::new();
    for _ in 0..2 {
        let tenant: Uuid = sqlx::query_scalar("INSERT INTO tenants(name) VALUES ($1) RETURNING id")
            .bind(Uuid::new_v4().to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
        let (key, public_id, digest) = generate_key();
        sqlx::query("INSERT INTO api_keys(tenant_id, public_id, secret_digest, scopes) VALUES ($1, $2, $3, $4)")
            .bind(tenant).bind(public_id).bind(digest)
            .bind(vec!["credentials:read", "credentials:write", "flows:write"])
            .execute(&pool).await.unwrap();
        keys.push(key);
        tenants.push(tenant);
    }
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let mut api = Command::new(env!("CARGO_BIN_EXE_api"))
        .env("DATABASE_URL", &database_url)
        .env("API_BIND", format!("127.0.0.1:{port}"))
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap();
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
    let create = client
        .post(format!("{base}/v1/provider-credentials"))
        .bearer_auth(&keys[0])
        .json(&json!({"provider_alias":"openai","secret_reference":format!("tenants/{}/openai", tenants[0])}))
        .send()
        .await
        .unwrap();
    assert_eq!(create.status(), StatusCode::CREATED);
    let credential: Value = create.json().await.unwrap();
    let id = credential["credential_id"].as_str().unwrap();
    assert!(credential.get("secret_reference").is_none());
    let other: Value = client
        .get(format!("{base}/v1/provider-credentials"))
        .bearer_auth(&keys[1])
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(other["credentials"], json!([]));
    let invalid = client
        .post(format!("{base}/v1/provider-credentials"))
        .bearer_auth(&keys[0])
        .json(&json!({"provider_alias":"openai","secret_reference":"../key"}))
        .send()
        .await
        .unwrap();
    assert_eq!(invalid.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let foreign_reference = client
        .post(format!("{base}/v1/provider-credentials"))
        .bearer_auth(&keys[1])
        .json(&json!({"provider_alias":"openai","secret_reference":format!("tenants/{}/openai", tenants[0])}))
        .send().await.unwrap();
    assert_eq!(foreign_reference.status(), StatusCode::UNPROCESSABLE_ENTITY);

    let mut flow: Value =
        serde_json::from_str(include_str!("../examples/basic-chat.json")).unwrap();
    flow["flow_id"] = json!(Uuid::new_v4());
    flow["nodes"][2]["config"]["model_alias"] = json!("openai");
    flow["nodes"][2]["config"]["credential_id"] = json!(id);
    let flow_id = flow["flow_id"].as_str().unwrap();
    let foreign_validation = client
        .post(format!("{base}/v1/flows/validate"))
        .bearer_auth(&keys[1])
        .json(&flow)
        .send()
        .await
        .unwrap();
    assert_eq!(
        foreign_validation.status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    let owned_validation = client
        .post(format!("{base}/v1/flows/validate"))
        .bearer_auth(&keys[0])
        .json(&flow)
        .send()
        .await
        .unwrap();
    assert_eq!(owned_validation.status(), StatusCode::OK);
    let foreign = client
        .post(format!("{base}/v1/flows/{flow_id}/versions"))
        .bearer_auth(&keys[1])
        .json(&json!({"name":"Foreign", "definition":flow}))
        .send()
        .await
        .unwrap();
    assert_eq!(foreign.status(), StatusCode::UNPROCESSABLE_ENTITY);
    let owned = client
        .post(format!("{base}/v1/flows/{flow_id}/versions"))
        .bearer_auth(&keys[0])
        .json(&json!({"name":"Owned", "definition":flow}))
        .send()
        .await
        .unwrap();
    assert_eq!(owned.status(), StatusCode::CREATED);
    let _ = api.kill();
    let _ = api.wait();
}
