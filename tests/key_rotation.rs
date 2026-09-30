use std::{
    process::{Child, Command, Stdio},
    time::Duration,
};

use chatflow::{auth::generate_key, flow::FlowDefinition};
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

#[tokio::test]
async fn rotation_preserves_access_and_shared_quotas_until_old_token_is_revoked() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let pool = PgPool::connect(&database_url).await.unwrap();
    sqlx::migrate!().run(&pool).await.unwrap();
    let tenant_id: Uuid = sqlx::query_scalar("INSERT INTO tenants(name) VALUES ($1) RETURNING id")
        .bind(Uuid::new_v4().to_string())
        .fetch_one(&pool)
        .await
        .unwrap();
    let (old_token, old_public_id, old_digest) = generate_key();
    let key_id: Uuid = sqlx::query_scalar("INSERT INTO api_keys(tenant_id, public_id, secret_digest, scopes, max_runs_per_minute) VALUES ($1, $2, $3, $4, 1) RETURNING id")
        .bind(tenant_id).bind(&old_public_id).bind(old_digest)
        .bind(vec!["keys:read", "keys:write", "flows:write", "runs:write", "runs:read"])
        .fetch_one(&pool).await.unwrap();
    let foreign_tenant: Uuid =
        sqlx::query_scalar("INSERT INTO tenants(name) VALUES ($1) RETURNING id")
            .bind(Uuid::new_v4().to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
    let (foreign_token, foreign_id, foreign_digest) = generate_key();
    sqlx::query(
        "INSERT INTO api_keys(tenant_id, public_id, secret_digest, scopes) VALUES ($1, $2, $3, $4)",
    )
    .bind(foreign_tenant)
    .bind(foreign_id)
    .bind(foreign_digest)
    .bind(vec!["keys:write"])
    .execute(&pool)
    .await
    .unwrap();

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let _api = Process(
        Command::new(env!("CARGO_BIN_EXE_api"))
            .env("DATABASE_URL", &database_url)
            .env("API_BIND", format!("127.0.0.1:{port}"))
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .unwrap(),
    );
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
    let mut flow: FlowDefinition =
        serde_json::from_str(include_str!("../examples/basic-chat.json")).unwrap();
    flow.flow_id = Uuid::new_v4();
    let published: Value = client
        .post(format!("{base}/v1/flows/{}/versions", flow.flow_id))
        .bearer_auth(&old_token)
        .json(&json!({"name":"Rotation quota", "definition":flow}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let version_id = published["version_id"].as_str().unwrap();
    let first = client
        .post(format!("{base}/v1/runs"))
        .bearer_auth(&old_token)
        .json(&json!({"flow_version_id":version_id,"input":{"message":"first"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::CREATED);
    let first: Value = first.json().await.unwrap();

    let rotated = client
        .post(format!("{base}/v1/api-keys/rotate"))
        .bearer_auth(&old_token)
        .send()
        .await
        .unwrap();
    assert_eq!(rotated.status(), StatusCode::CREATED);
    let body: Value = rotated.json().await.unwrap();
    let new_token = body["api_key"].as_str().unwrap();
    let new_public_id = body["public_id"].as_str().unwrap();
    assert_eq!(body["key_id"], key_id.to_string());
    assert_ne!(new_token, old_token);
    let conversation = first["conversation_id"].as_str().unwrap();
    let rotated_owner_read = client
        .get(format!("{base}/v1/conversations/{conversation}/variables"))
        .bearer_auth(new_token)
        .send()
        .await
        .unwrap();
    assert_eq!(rotated_owner_read.status(), StatusCode::OK);
    let listed: Value = client
        .get(format!("{base}/v1/api-keys/tokens"))
        .bearer_auth(new_token)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed["tokens"].as_array().unwrap().len(), 2);
    assert!(!listed.to_string().contains(new_token));
    let second = client
        .post(format!("{base}/v1/runs"))
        .bearer_auth(new_token)
        .json(&json!({"flow_version_id":version_id,"input":{"message":"second"}}))
        .send()
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::TOO_MANY_REQUESTS);
    let foreign_revoke = client
        .post(format!("{base}/v1/api-keys/tokens/{old_public_id}/revoke"))
        .bearer_auth(&foreign_token)
        .send()
        .await
        .unwrap();
    assert_eq!(foreign_revoke.status(), StatusCode::NOT_FOUND);
    let revoked = client
        .post(format!("{base}/v1/api-keys/tokens/{old_public_id}/revoke"))
        .bearer_auth(new_token)
        .send()
        .await
        .unwrap();
    assert_eq!(revoked.status(), StatusCode::OK);
    let old_check = client
        .get(format!("{base}/v1/api-keys/tokens"))
        .bearer_auth(&old_token)
        .send()
        .await
        .unwrap();
    assert_eq!(old_check.status(), StatusCode::UNAUTHORIZED);
    let new_check = client
        .get(format!("{base}/v1/api-keys/tokens"))
        .bearer_auth(new_token)
        .send()
        .await
        .unwrap();
    assert_eq!(new_check.status(), StatusCode::OK);
    let new_revoke = client
        .post(format!("{base}/v1/api-keys/tokens/{new_public_id}/revoke"))
        .bearer_auth(new_token)
        .send()
        .await
        .unwrap();
    assert_eq!(new_revoke.status(), StatusCode::OK);
    let final_check = client
        .get(format!("{base}/v1/api-keys/tokens"))
        .bearer_auth(new_token)
        .send()
        .await
        .unwrap();
    assert_eq!(final_check.status(), StatusCode::UNAUTHORIZED);
}
