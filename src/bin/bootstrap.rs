use chatflow::auth::generate_key;
use sqlx::PgPool;
use uuid::Uuid;

const APPLICATION_SCOPES: &[&str] = &[
    "flows:read",
    "flows:write",
    "runs:write",
    "runs:read",
    "rag:read",
    "credentials:read",
    "credentials:write",
    "keys:read",
    "keys:write",
];
const RAG_CONFIG_SCOPES: &[&str] = &["rag:configure"];
const OPS_SCOPES: &[&str] = &["ops:read"];

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let name = std::env::args()
        .nth(1)
        .ok_or("usage: cargo run --example bootstrap -- TENANT_NAME")?;
    let database_url = std::env::var("DATABASE_URL")?;
    let pool = PgPool::connect(&database_url).await?;
    sqlx::migrate!().run(&pool).await?;
    let mut tx = pool.begin().await?;
    let tenant_id: Uuid = sqlx::query_scalar("INSERT INTO tenants(name) VALUES ($1) RETURNING id")
        .bind(name)
        .fetch_one(&mut *tx)
        .await?;
    let mut keys = Vec::new();
    for (label, scopes) in [
        ("api_key", APPLICATION_SCOPES),
        ("rag_config_key", RAG_CONFIG_SCOPES),
        ("ops_api_key", OPS_SCOPES),
    ] {
        let (token, public_id, digest) = generate_key();
        sqlx::query(
            "INSERT INTO api_keys(tenant_id, public_id, secret_digest, scopes) VALUES ($1, $2, $3, $4)",
        )
        .bind(tenant_id)
        .bind(public_id)
        .bind(digest)
        .bind(scopes)
        .execute(&mut *tx)
        .await?;
        keys.push((label, token));
    }
    tx.commit().await?;
    println!("tenant_id: {tenant_id}");
    for (label, token) in keys {
        println!("{label}: {token}");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{APPLICATION_SCOPES, OPS_SCOPES, RAG_CONFIG_SCOPES};

    #[test]
    fn local_keys_keep_dataset_assignment_and_metrics_separate() {
        assert!(APPLICATION_SCOPES.contains(&"rag:read"));
        assert!(!APPLICATION_SCOPES.contains(&"rag:configure"));
        assert!(!APPLICATION_SCOPES.contains(&"ops:read"));
        assert_eq!(RAG_CONFIG_SCOPES, ["rag:configure"]);
        assert_eq!(OPS_SCOPES, ["ops:read"]);
    }
}
