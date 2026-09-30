use std::{
    process::{Child, Command, Stdio},
    time::Duration,
};

use chatflow::{auth::generate_key, flow::FlowDefinition};
use reqwest::{Client, StatusCode};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

struct Api(Child);

impl Drop for Api {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

async fn make_key(pool: &PgPool, tenant_id: Uuid) -> String {
    let (key, public_id, digest) = generate_key();
    sqlx::query(
        "INSERT INTO api_keys(tenant_id, public_id, secret_digest, scopes) VALUES ($1, $2, $3, $4)",
    )
    .bind(tenant_id)
    .bind(public_id)
    .bind(digest)
    .bind(vec!["flows:read", "flows:write", "runs:read", "runs:write"])
    .execute(pool)
    .await
    .unwrap();
    key
}

#[tokio::test]
async fn conversation_browser_and_notes_keep_key_and_tenant_boundaries() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let pool = PgPool::connect(&database_url).await.unwrap();
    sqlx::migrate!().run(&pool).await.unwrap();
    let tenant_id: Uuid = sqlx::query_scalar("INSERT INTO tenants(name) VALUES ($1) RETURNING id")
        .bind(format!("workbench-{}", Uuid::new_v4()))
        .fetch_one(&pool)
        .await
        .unwrap();
    let other_tenant_id: Uuid =
        sqlx::query_scalar("INSERT INTO tenants(name) VALUES ($1) RETURNING id")
            .bind(format!("workbench-other-{}", Uuid::new_v4()))
            .fetch_one(&pool)
            .await
            .unwrap();
    let owner_key = make_key(&pool, tenant_id).await;
    let other_family_key = make_key(&pool, tenant_id).await;
    let other_tenant_key = make_key(&pool, other_tenant_id).await;

    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let _api = Api(Command::new(env!("CARGO_BIN_EXE_api"))
        .env("DATABASE_URL", &database_url)
        .env("API_BIND", format!("127.0.0.1:{port}"))
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .spawn()
        .unwrap());
    let client = Client::new();
    let base = format!("http://127.0.0.1:{port}");
    for _ in 0..100 {
        if client
            .get(format!("{base}/health/ready"))
            .send()
            .await
            .is_ok_and(|response| response.status().is_success())
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
        .bearer_auth(&owner_key)
        .json(&json!({"name":"Workbench test","definition":flow}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let run: Value = client
        .post(format!("{base}/v1/runs"))
        .bearer_auth(&owner_key)
        .json(&json!({"flow_version_id":published["version_id"],"input":{"message":"hello"}}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let conversation_id = run["conversation_id"].as_str().unwrap();
    let conversations: Value = client
        .get(format!("{base}/v1/conversations"))
        .bearer_auth(&owner_key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(conversations["conversations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["conversation_id"] == conversation_id));
    let runs: Value = client
        .get(format!("{base}/v1/conversations/{conversation_id}/runs"))
        .bearer_auth(&owner_key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(runs["runs"][0]["run_id"], run["run_id"]);

    let notes_url = format!("{base}/v1/conversations/{conversation_id}/notes");
    for body in ["  ", &"é".repeat(5_001)] {
        assert_eq!(
            client
                .post(&notes_url)
                .bearer_auth(&owner_key)
                .json(&json!({"body":body}))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::BAD_REQUEST
        );
    }
    let created = client
        .post(&notes_url)
        .bearer_auth(&owner_key)
        .json(&json!({"body":"Check the answer"}))
        .send()
        .await
        .unwrap();
    assert_eq!(created.status(), StatusCode::CREATED);
    let note: Value = created.json().await.unwrap();
    let note_url = format!("{notes_url}/{}", note["note_id"].as_str().unwrap());
    let updated: Value = client
        .patch(&note_url)
        .bearer_auth(&owner_key)
        .json(&json!({"body":"Reviewed"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(updated["body"], "Reviewed");

    for key in [&other_family_key, &other_tenant_key] {
        let listed: Value = client
            .get(format!("{base}/v1/conversations"))
            .bearer_auth(key)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert!(listed["conversations"].as_array().unwrap().is_empty());
        for path in [
            format!("{base}/v1/conversations/{conversation_id}/runs"),
            notes_url.clone(),
        ] {
            assert_eq!(
                client
                    .get(path)
                    .bearer_auth(key)
                    .send()
                    .await
                    .unwrap()
                    .status(),
                StatusCode::NOT_FOUND
            );
        }
        assert_eq!(
            client
                .patch(&note_url)
                .bearer_auth(key)
                .json(&json!({"body":"intrusion"}))
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
    }

    let listed: Value = client
        .get(&notes_url)
        .bearer_auth(&owner_key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(listed["notes"].as_array().unwrap().len(), 1);
    assert_eq!(listed["notes"][0]["body"], "Reviewed");
    assert_eq!(
        client
            .delete(&note_url)
            .bearer_auth(&owner_key)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
    assert_eq!(
        client
            .delete(&note_url)
            .bearer_auth(&owner_key)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );

    let public_id = owner_key
        .strip_prefix("cf_")
        .unwrap()
        .split('.')
        .next()
        .unwrap();
    sqlx::query("UPDATE api_keys SET allowed_flow_ids = ARRAY[]::uuid[] WHERE tenant_id = $1 AND public_id = $2")
        .bind(tenant_id)
        .bind(public_id)
        .execute(&pool)
        .await
        .unwrap();
    let hidden: Value = client
        .get(format!("{base}/v1/conversations"))
        .bearer_auth(&owner_key)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert!(hidden["conversations"].as_array().unwrap().is_empty());
    assert_eq!(
        client
            .get(&notes_url)
            .bearer_auth(&owner_key)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
}
