use std::{
    process::{Child, Command, Stdio},
    time::Duration,
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

fn spawn(binary: &str, database_url: &str, port: Option<u16>) -> Process {
    let mut command = Command::new(binary);
    command
        .env("DATABASE_URL", database_url)
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());
    if let Some(port) = port {
        command.env("API_BIND", format!("127.0.0.1:{port}"));
    }
    Process(command.spawn().unwrap())
}

async fn new_key(pool: &PgPool, tenant_id: Uuid) -> String {
    let (key, public_id, digest) = generate_key();
    sqlx::query(
        "INSERT INTO api_keys(tenant_id, public_id, secret_digest, scopes) VALUES ($1, $2, $3, $4)",
    )
    .bind(tenant_id)
    .bind(public_id)
    .bind(digest)
    .bind(vec!["flows:write", "runs:write", "runs:read"])
    .execute(pool)
    .await
    .unwrap();
    key
}

async fn publish(client: &Client, base: &str, key: &str, definition: Value) -> String {
    let flow_id = definition["flow_id"].as_str().unwrap();
    let response = client
        .post(format!("{base}/v1/flows/{flow_id}/versions"))
        .bearer_auth(key)
        .json(&json!({"name":"Variable test", "definition": definition}))
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "{}",
        response.text().await.unwrap()
    );
    response.json::<Value>().await.unwrap()["version_id"]
        .as_str()
        .unwrap()
        .to_owned()
}

async fn create_run(
    client: &Client,
    base: &str,
    key: &str,
    version_id: &str,
    conversation_id: Option<&str>,
    input: Value,
) -> Value {
    let response = client
        .post(format!("{base}/v1/runs"))
        .bearer_auth(key)
        .json(
            &json!({"flow_version_id":version_id,"conversation_id":conversation_id,"input":input}),
        )
        .send()
        .await
        .unwrap();
    assert_eq!(
        response.status(),
        StatusCode::CREATED,
        "{}",
        response.text().await.unwrap()
    );
    response.json().await.unwrap()
}

async fn completed(client: &Client, base: &str, key: &str, run_id: &str) -> Value {
    for _ in 0..100 {
        let run: Value = client
            .get(format!("{base}/v1/runs/{run_id}"))
            .bearer_auth(key)
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        if run["status"] == "succeeded" {
            return run;
        }
        assert_ne!(run["status"], "failed", "{run}");
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    panic!("run did not finish");
}

#[tokio::test]
async fn variables_are_private_to_one_key_and_one_conversation() {
    let Ok(database_url) = std::env::var("DATABASE_URL") else {
        return;
    };
    let pool = PgPool::connect(&database_url).await.unwrap();
    sqlx::migrate!().run(&pool).await.unwrap();
    let tenant_id: Uuid = sqlx::query_scalar("INSERT INTO tenants(name) VALUES ($1) RETURNING id")
        .bind(format!("variable-test-{}", Uuid::new_v4()))
        .fetch_one(&pool)
        .await
        .unwrap();
    let alice = new_key(&pool, tenant_id).await;
    let bob = new_key(&pool, tenant_id).await;
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let _api = spawn(env!("CARGO_BIN_EXE_api"), &database_url, Some(port));
    let _worker = spawn(env!("CARGO_BIN_EXE_worker"), &database_url, None);
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

    let set_flow = Uuid::new_v4();
    let setter = publish(&client, &base, &alice, json!({
        "schema_version":1,"flow_id":set_flow,"inputs":{"message":"string"},
        "outputs":{"reply":{"node":"reply","output":"value"}},
        "nodes":[
            {"id":"input","type":"input","config":{"name":"message"}},
            {"id":"save","type":"variable.set","config":{"name":"topic"},"bindings":{"value":{"node":"input","output":"value"}}},
            {"id":"reply","type":"output","config":{},"bindings":{"value":{"node":"save","output":"value"}}}
        ],
        "edges":[
            {"from":{"node":"input","output":"value"},"to_node":"save","to_input":"value"},
            {"from":{"node":"save","output":"value"},"to_node":"reply","to_input":"value"}
        ],"limits":{}
    })).await;
    let first = create_run(
        &client,
        &base,
        &alice,
        &setter,
        None,
        json!({"message":"ocean"}),
    )
    .await;
    let conversation = first["conversation_id"].as_str().unwrap();
    completed(&client, &base, &alice, first["run_id"].as_str().unwrap()).await;
    let variables: Value = client
        .get(format!("{base}/v1/conversations/{conversation}/variables"))
        .bearer_auth(&alice)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(variables["variables"]["topic"], "ocean");

    let get_flow = Uuid::new_v4();
    let getter = publish(&client, &base, &alice, json!({
        "schema_version":1,"flow_id":get_flow,"inputs":{},
        "outputs":{"reply":{"node":"reply","output":"value"}},
        "nodes":[
            {"id":"read","type":"variable.get","config":{"name":"topic"}},
            {"id":"reply","type":"output","config":{},"bindings":{"value":{"node":"read","output":"value"}}}
        ],
        "edges":[{"from":{"node":"read","output":"value"},"to_node":"reply","to_input":"value"}],
        "limits":{}
    })).await;
    let read = create_run(
        &client,
        &base,
        &alice,
        &getter,
        Some(conversation),
        json!({}),
    )
    .await;
    let result = completed(&client, &base, &alice, read["run_id"].as_str().unwrap()).await;
    assert_eq!(result["output"]["reply"], "ocean");

    for path in [
        format!("/v1/conversations/{conversation}/variables"),
        format!("/v1/conversations/{conversation}/messages"),
        format!("/v1/runs/{}", first["run_id"].as_str().unwrap()),
    ] {
        assert_eq!(
            client
                .get(format!("{base}{path}"))
                .bearer_auth(&bob)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::NOT_FOUND
        );
    }
    assert_eq!(
        client
            .put(format!(
                "{base}/v1/conversations/{conversation}/variables/topic"
            ))
            .bearer_auth(&bob)
            .json(&json!({"value":"stolen"}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(client.post(format!("{base}/v1/runs")).bearer_auth(&bob)
        .json(&json!({"flow_version_id":setter,"conversation_id":conversation,"input":{"message":"stolen"}}))
        .send().await.unwrap().status(), StatusCode::NOT_FOUND);

    let second = create_run(
        &client,
        &base,
        &alice,
        &setter,
        None,
        json!({"message":"forest"}),
    )
    .await;
    completed(&client, &base, &alice, second["run_id"].as_str().unwrap()).await;
    let second_conversation = second["conversation_id"].as_str().unwrap();
    let first_vars: Value = client
        .get(format!("{base}/v1/conversations/{conversation}/variables"))
        .bearer_auth(&alice)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let second_vars: Value = client
        .get(format!(
            "{base}/v1/conversations/{second_conversation}/variables"
        ))
        .bearer_auth(&alice)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(first_vars["variables"]["topic"], "ocean");
    assert_eq!(second_vars["variables"]["topic"], "forest");
    assert_eq!(
        client
            .put(format!(
                "{base}/v1/conversations/{conversation}/variables/count"
            ))
            .bearer_auth(&alice)
            .json(&json!({"value":7}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        client
            .put(format!(
                "{base}/v1/conversations/{conversation}/variables/bad-name"
            ))
            .bearer_auth(&alice)
            .json(&json!({"value":"ignored"}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        client
            .put(format!(
                "{base}/v1/conversations/{conversation}/variables/oversized"
            ))
            .bearer_auth(&alice)
            .json(&json!({"value":"x".repeat(16_385)}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(
        client
            .delete(format!(
                "{base}/v1/conversations/{conversation}/variables/count"
            ))
            .bearer_auth(&alice)
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::NO_CONTENT
    );
}
