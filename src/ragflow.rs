use std::{collections::HashSet, time::Duration};

use anyhow::{anyhow, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct Source {
    pub dataset_id: String,
    pub document_id: String,
    pub chunk_id: String,
    pub content: String,
}

pub struct RagflowClient {
    base_url: String,
    api_key: String,
    pool: PgPool,
    http: reqwest::Client,
}

impl RagflowClient {
    pub fn from_env(pool: PgPool) -> Result<Self> {
        Self::new(
            std::env::var("RAGFLOW_BASE_URL").context("RAGFLOW_BASE_URL is required")?,
            load_api_key(
                std::env::var("RAGFLOW_API_KEY").ok(),
                std::env::var("RAGFLOW_API_KEY_FILE").ok(),
            )?,
            pool,
        )
    }

    pub fn new(base_url: String, api_key: String, pool: PgPool) -> Result<Self> {
        let url = reqwest::Url::parse(&base_url).context("invalid RAGFLOW_BASE_URL")?;
        let secure = url.scheme() == "https"
            || (url.scheme() == "http"
                && (crate::config::allows_local_http(url.host_str(), "ragflow-cpu")
                    || crate::config::allows_local_http(url.host_str(), "host.docker.internal")));
        if !secure
            || url.username() != ""
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return Err(anyhow!(
                "RAGFLOW_BASE_URL must be HTTPS or local HTTP without credentials or query"
            ));
        }
        if api_key.is_empty() {
            return Err(anyhow!("RAGFLOW_API_KEY is empty"));
        }
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            api_key,
            pool,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(30))
                .build()?,
        })
    }

    pub async fn retrieve(
        &self,
        tenant_id: Uuid,
        question: &str,
        limit: usize,
    ) -> Result<Vec<Source>> {
        if question.trim().is_empty() || question.len() > 16_384 || !(1..=10).contains(&limit) {
            return Err(anyhow!("invalid RAGFlow retrieval query or limit"));
        }
        let datasets = sqlx::query_scalar::<_, String>(
            "SELECT dataset_id FROM ragflow_datasets WHERE tenant_id = $1 ORDER BY dataset_id",
        )
        .bind(tenant_id)
        .fetch_all(&self.pool)
        .await?;
        if datasets.is_empty() {
            return Err(anyhow!("no RAGFlow datasets configured for tenant"));
        }
        let response = self.http
            .post(format!("{}/api/v1/retrieval", self.base_url))
            .bearer_auth(&self.api_key)
            .json(&json!({"question": question, "dataset_ids": datasets, "page": 1, "page_size": limit}))
            .send().await.context("RAGFlow retrieval request failed")?;
        if !response.status().is_success() {
            return Err(anyhow!("RAGFlow returned HTTP {}", response.status()));
        }
        let body: Value = response
            .json()
            .await
            .context("invalid RAGFlow response JSON")?;
        parse_retrieval(&body, &datasets, limit)
    }
}

fn load_api_key(direct: Option<String>, file: Option<String>) -> Result<String> {
    let key = if let Some(path) = file {
        std::fs::read_to_string(&path)
            .with_context(|| format!("cannot read RAGFLOW_API_KEY_FILE at {path}"))?
    } else {
        direct.context("RAGFLOW_API_KEY or RAGFLOW_API_KEY_FILE is required")?
    };
    let key = key.trim_end_matches(['\r', '\n']).to_owned();
    if key.is_empty() || key.len() > 512 || !key.bytes().all(|byte| (33..=126).contains(&byte)) {
        return Err(anyhow!("invalid RAGFlow API key"));
    }
    Ok(key)
}

pub fn validate_dataset_ids(ids: &[String]) -> Result<()> {
    let mut seen = HashSet::new();
    if ids.len() > 32
        || ids.iter().any(|id| {
            id.is_empty()
                || id.len() > 128
                || !id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
                || !seen.insert(id)
        })
    {
        return Err(anyhow!("invalid RAGFlow dataset IDs"));
    }
    Ok(())
}

fn parse_retrieval(body: &Value, allowed_datasets: &[String], limit: usize) -> Result<Vec<Source>> {
    if body.get("code").and_then(Value::as_i64) != Some(0) {
        return Err(anyhow!(
            "RAGFlow retrieval returned a nonzero business code"
        ));
    }
    let chunks = body
        .pointer("/data/chunks")
        .and_then(Value::as_array)
        .context("RAGFlow response has no chunks array")?;
    chunks
        .iter()
        .take(limit)
        .map(|chunk| {
            let get = |name: &str| -> Result<&str> {
                chunk
                    .get(name)
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .with_context(|| format!("RAGFlow chunk missing {name}"))
            };
            let dataset_id = get("dataset_id")?;
            if !allowed_datasets.iter().any(|id| id == dataset_id) {
                return Err(anyhow!(
                    "RAGFlow returned a chunk outside the tenant's datasets"
                ));
            }
            let content = get("content")?;
            if content.chars().count() > 4_000 {
                return Err(anyhow!("RAGFlow chunk exceeds context limit"));
            }
            Ok(Source {
                dataset_id: dataset_id.to_owned(),
                document_id: get("document_id")?.to_owned(),
                chunk_id: get("id")?.to_owned(),
                content: content.to_owned(),
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{load_api_key, parse_retrieval, validate_dataset_ids, RagflowClient};
    use serde_json::json;
    use sqlx::postgres::PgPoolOptions;

    #[tokio::test]
    async fn config_requires_safe_base_url() {
        let pool = PgPoolOptions::new()
            .connect_lazy("postgres://localhost/chatflow")
            .unwrap();
        assert!(RagflowClient::new(
            "https://ragflow.example".into(),
            "secret".into(),
            pool.clone()
        )
        .is_ok());
        assert!(RagflowClient::new(
            "http://ragflow.example".into(),
            "secret".into(),
            pool.clone()
        )
        .is_err());
        assert!(RagflowClient::new("https://ragflow.example".into(), "".into(), pool).is_err());
    }

    #[test]
    fn dataset_ids_are_bounded_and_unique() {
        assert!(validate_dataset_ids(&["dataset_1".into()]).is_ok());
        assert!(validate_dataset_ids(&[]).is_ok());
        assert!(validate_dataset_ids(&["dataset_1".into(), "dataset_1".into()]).is_err());
        assert!(validate_dataset_ids(&["invalid/id".into()]).is_err());
        assert!(validate_dataset_ids(&vec!["id".into(); 33]).is_err());
    }

    #[test]
    fn api_key_file_has_precedence_and_is_bounded() {
        let path = std::env::temp_dir().join(format!("ragflow-key-{}", uuid::Uuid::new_v4()));
        std::fs::write(&path, "file-secret\n").unwrap();
        assert_eq!(
            load_api_key(
                Some("direct-secret".into()),
                Some(path.to_string_lossy().into())
            )
            .unwrap(),
            "file-secret"
        );
        std::fs::write(&path, "bad\nkey").unwrap();
        assert!(load_api_key(None, Some(path.to_string_lossy().into())).is_err());
        std::fs::remove_file(path).unwrap();
        assert!(load_api_key(None, None).is_err());
    }

    #[test]
    fn response_checks_business_code_and_dataset_scope() {
        let allowed = vec!["dataset_a".to_owned()];
        let chunk = json!({"id":"chunk_1", "document_id":"doc_1", "dataset_id":"dataset_a", "content":"Ocean notes"});
        let body = json!({"code":0,"data":{"chunks":[chunk]}});
        let sources = parse_retrieval(&body, &allowed, 1).unwrap();
        assert_eq!(sources[0].document_id, "doc_1");
        assert_eq!(sources[0].chunk_id, "chunk_1");
        assert!(parse_retrieval(&json!({"code":102,"data":{"chunks":[]}}), &allowed, 1).is_err());
        assert!(parse_retrieval(&json!({"code":0,"data":{"chunks":[{"id":"c","document_id":"d","dataset_id":"other","content":"private"}]}}), &allowed, 1).is_err());
    }
}
