use std::{future::Future, pin::Pin};

use anyhow::{anyhow, Result};
use futures_util::StreamExt;
use rig_core::{
    client::CompletionClient, completion::CompletionModel, providers::openai,
    streaming::StreamedAssistantContent,
};
use sqlx::PgPool;
use thiserror::Error;
use uuid::Uuid;

pub type ProviderFuture<'a> =
    Pin<Box<dyn Future<Output = Result<ChatResponse, ProviderError>> + Send + 'a>>;
pub type SinkFuture<'a> = Pin<Box<dyn Future<Output = Result<(), ProviderError>> + Send + 'a>>;

pub trait TokenSink: Send + Sync {
    fn emit<'a>(&'a self, text: String) -> SinkFuture<'a>;
    fn usage<'a>(
        &'a self,
        _input_tokens: Option<u32>,
        _output_tokens: Option<u32>,
    ) -> SinkFuture<'a> {
        Box::pin(async { Ok(()) })
    }
}

#[derive(Clone, Debug)]
pub struct ChatRequest {
    pub tenant_id: Uuid,
    pub model_alias: String,
    pub credential_id: Option<Uuid>,
    pub messages: Vec<ChatMessage>,
    pub max_output_tokens: u32,
}

#[derive(Clone, Debug)]
pub struct ChatMessage {
    pub role: ChatRole,
    pub content: String,
}

#[derive(Clone, Debug)]
pub enum ChatRole {
    System,
    User,
    Assistant,
}

#[derive(Clone, Debug)]
pub struct ChatResponse {
    pub content: String,
    pub input_tokens: Option<u32>,
    pub output_tokens: Option<u32>,
}

#[derive(Debug, Error)]
pub enum ProviderError {
    #[error("model alias is not configured: {0}")]
    UnknownModel(String),
    #[error("model request failed: {0}")]
    Request(String),
}

/// Implementations resolve a server-configured alias. Flow JSON never supplies a URL or secret.
pub trait ModelProvider: Send + Sync {
    fn chat<'a>(&'a self, request: ChatRequest, sink: &'a dyn TokenSink) -> ProviderFuture<'a>;
}

#[derive(Default)]
pub struct MockProvider;

impl ModelProvider for MockProvider {
    fn chat<'a>(&'a self, request: ChatRequest, sink: &'a dyn TokenSink) -> ProviderFuture<'a> {
        Box::pin(async move {
            if request.model_alias != "mock" {
                return Err(ProviderError::UnknownModel(request.model_alias));
            }
            let prompt = request
                .messages
                .iter()
                .rev()
                .find(|message| matches!(message.role, ChatRole::User))
                .map(|message| message.content.as_str())
                .unwrap_or("");
            let content = format!("Mock reply to: {prompt}")
                .split_whitespace()
                .take(request.max_output_tokens as usize)
                .collect::<Vec<_>>()
                .join(" ");
            for word in content.split_inclusive(char::is_whitespace) {
                sink.emit(word.to_owned()).await?;
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
            sink.usage(
                Some(prompt.split_whitespace().count() as u32),
                Some(content.split_whitespace().count() as u32),
            )
            .await?;
            Ok(ChatResponse {
                input_tokens: Some(prompt.split_whitespace().count() as u32),
                output_tokens: Some(content.split_whitespace().count() as u32),
                content,
            })
        })
    }
}

pub struct RouterProvider {
    pool: PgPool,
    vault: Option<VaultClient>,
    openai_model: Option<String>,
    openai_base_url: String,
    mock: MockProvider,
}

const DEFAULT_OPENAI_BASE_URL: &str = "https://api.openai.com/v1";

fn validate_openai_base_url(raw: &str) -> Result<String> {
    let url = reqwest::Url::parse(raw)?;
    let local_http =
        url.scheme() == "http" && matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "::1"));
    if !(url.scheme() == "https" || local_http)
        || url.host_str().is_none()
        || url.username() != ""
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(anyhow!(
            "OPENAI_BASE_URL must be HTTPS or local HTTP without credentials, query, or fragment"
        ));
    }
    Ok(raw.trim_end_matches('/').to_owned())
}

fn build_openai_client(
    key: &str,
    base_url: &str,
) -> Result<openai::CompletionsClient, ProviderError> {
    let http = reqwest::Client::builder()
        .user_agent("chatflow/0.1")
        .redirect(reqwest::redirect::Policy::none())
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|_| ProviderError::Request("model HTTP client initialization failed".into()))?;
    openai::Client::builder()
        .api_key(key)
        .base_url(base_url)
        .http_client(http)
        .build()
        .map(openai::Client::completions_api)
        .map_err(|_| ProviderError::Request("model client initialization failed".into()))
}

struct VaultClient {
    base_url: String,
    token: String,
    http: reqwest::Client,
}

impl VaultClient {
    fn new(base_url: String, token: String) -> Result<Self> {
        let url = reqwest::Url::parse(&base_url)?;
        let local_http =
            url.scheme() == "http" && crate::config::allows_local_http(url.host_str(), "vault");
        if !(url.scheme() == "https" || local_http)
            || url.username() != ""
            || url.password().is_some()
            || url.query().is_some()
            || url.fragment().is_some()
            || url.path() != "/"
            || token.is_empty()
        {
            return Err(anyhow!(
                "VAULT_ADDR must be HTTPS or local HTTP; VAULT_TOKEN must be set"
            ));
        }
        Ok(Self {
            base_url: base_url.trim_end_matches('/').to_owned(),
            token,
            http: reqwest::Client::builder()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(std::time::Duration::from_secs(10))
                .build()?,
        })
    }

    async fn read_api_key(&self, reference: &str) -> Result<String, ProviderError> {
        if !valid_secret_reference(reference) {
            return Err(ProviderError::Request(
                "invalid credential reference".into(),
            ));
        }
        let response = self
            .http
            .get(format!("{}/v1/secret/data/{reference}", self.base_url))
            .header("X-Vault-Token", &self.token)
            .send()
            .await
            .map_err(|_| ProviderError::Request("Vault request failed".into()))?;
        if !response.status().is_success() {
            return Err(ProviderError::Request(format!(
                "Vault returned HTTP {}",
                response.status()
            )));
        }
        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|_| ProviderError::Request("invalid Vault response".into()))?;
        body.pointer("/data/data/api_key")
            .and_then(serde_json::Value::as_str)
            .filter(|key| !key.is_empty())
            .map(str::to_owned)
            .ok_or_else(|| ProviderError::Request("credential has no api_key".into()))
    }
}

pub fn valid_secret_reference(reference: &str) -> bool {
    !reference.is_empty()
        && reference.len() <= 256
        && reference.split('/').all(|segment| {
            !segment.is_empty()
                && segment != "."
                && segment != ".."
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
        })
}

pub fn valid_tenant_secret_reference(tenant_id: Uuid, reference: &str) -> bool {
    valid_secret_reference(reference)
        && reference
            .strip_prefix(&format!("tenants/{tenant_id}/"))
            .is_some_and(|suffix| !suffix.is_empty())
}

impl RouterProvider {
    pub fn from_env(pool: PgPool) -> Result<Self> {
        let vault = match (
            std::env::var("VAULT_ADDR").ok(),
            std::env::var("VAULT_TOKEN").ok(),
        ) {
            (Some(address), Some(token)) => Some(VaultClient::new(address, token)?),
            (None, None) => None,
            _ => return Err(anyhow!("VAULT_ADDR and VAULT_TOKEN must be set together")),
        };
        Ok(Self {
            pool,
            vault,
            openai_model: std::env::var("OPENAI_MODEL").ok(),
            openai_base_url: validate_openai_base_url(
                &std::env::var("OPENAI_BASE_URL")
                    .unwrap_or_else(|_| DEFAULT_OPENAI_BASE_URL.to_owned()),
            )?,
            mock: MockProvider,
        })
    }

    async fn resolve_openai_key(
        &self,
        tenant_id: Uuid,
        credential_id: Uuid,
    ) -> Result<String, ProviderError> {
        let reference: Option<String> = sqlx::query_scalar("SELECT secret_reference FROM provider_credentials WHERE tenant_id = $1 AND id = $2 AND provider_alias = 'openai'")
            .bind(tenant_id).bind(credential_id).fetch_optional(&self.pool).await
            .map_err(|_| ProviderError::Request("credential lookup failed".into()))?;
        let reference =
            reference.ok_or_else(|| ProviderError::Request("credential is unavailable".into()))?;
        if !valid_tenant_secret_reference(tenant_id, &reference) {
            return Err(ProviderError::Request(
                "credential reference is outside tenant path".into(),
            ));
        }
        let vault = self
            .vault
            .as_ref()
            .ok_or_else(|| ProviderError::Request("Vault is not configured".into()))?;
        vault.read_api_key(&reference).await
    }
}

impl ModelProvider for RouterProvider {
    fn chat<'a>(&'a self, request: ChatRequest, sink: &'a dyn TokenSink) -> ProviderFuture<'a> {
        if request.model_alias == "mock" {
            return self.mock.chat(request, sink);
        }
        Box::pin(async move {
            if request.model_alias != "openai" {
                return Err(ProviderError::UnknownModel(request.model_alias));
            }
            let credential_id = request
                .credential_id
                .ok_or_else(|| ProviderError::Request("credential ID is required".into()))?;
            let key = self
                .resolve_openai_key(request.tenant_id, credential_id)
                .await?;
            let model_name = self
                .openai_model
                .as_ref()
                .ok_or_else(|| ProviderError::UnknownModel("openai".into()))?;
            let client = build_openai_client(&key, &self.openai_base_url)?;
            let model = client.completion_model(model_name);
            let prompt = request
                .messages
                .iter()
                .rev()
                .find(|message| matches!(message.role, ChatRole::User))
                .map(|message| message.content.clone())
                .unwrap_or_default();
            let mut response = model
                .completion_request(prompt)
                .max_tokens(request.max_output_tokens as u64)
                .stream()
                .await
                .map_err(|e| ProviderError::Request(e.to_string()))?;
            let mut content = String::new();
            let mut usage = None;
            while let Some(part) = response.next().await {
                match part.map_err(|e| ProviderError::Request(e.to_string()))? {
                    StreamedAssistantContent::Text(text) => {
                        content.push_str(&text.text);
                        sink.emit(text.text).await?;
                    }
                    StreamedAssistantContent::Final(final_response) => {
                        usage = Some(final_response.usage)
                    }
                    _ => {}
                }
            }
            let usage = usage.ok_or_else(|| {
                ProviderError::Request("model stream ended without a final response".into())
            })?;
            sink.usage(
                u32::try_from(usage.input_tokens).ok(),
                u32::try_from(usage.output_tokens).ok(),
            )
            .await?;
            Ok(ChatResponse {
                content,
                input_tokens: u32::try_from(usage.input_tokens).ok(),
                output_tokens: u32::try_from(usage.output_tokens).ok(),
            })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        build_openai_client, valid_secret_reference, valid_tenant_secret_reference,
        validate_openai_base_url, MockProvider, RouterProvider, VaultClient,
        DEFAULT_OPENAI_BASE_URL,
    };
    use axum::{
        http::HeaderMap,
        routing::{get, post},
        Json, Router,
    };
    use futures_util::StreamExt;
    use rig_core::{
        client::CompletionClient, completion::CompletionModel, streaming::StreamedAssistantContent,
    };
    use serde_json::{json, Value};
    use sqlx::PgPool;
    use uuid::Uuid;

    #[test]
    fn vault_configuration_and_reference_are_bounded() {
        assert!(VaultClient::new("http://127.0.0.1:8200".into(), "token".into()).is_ok());
        assert!(VaultClient::new("http://vault.internal:8200".into(), "token".into()).is_err());
        assert!(VaultClient::new("https://vault.internal".into(), "".into()).is_err());
        assert!(valid_secret_reference("tenants/acme/openai_1"));
        let tenant = Uuid::new_v4();
        assert!(valid_tenant_secret_reference(
            tenant,
            &format!("tenants/{tenant}/openai")
        ));
        assert!(!valid_tenant_secret_reference(
            tenant,
            &format!("tenants/{}/openai", Uuid::new_v4())
        ));
        for bad in ["", "../key", "tenant//key", "key?x", "key%2Fother"] {
            assert!(!valid_secret_reference(bad));
        }
    }

    #[test]
    fn custom_openai_endpoint_requires_safe_server_url() {
        assert_eq!(
            validate_openai_base_url("https://router.example/openai/").unwrap(),
            "https://router.example/openai"
        );
        assert!(validate_openai_base_url("http://127.0.0.1:9000/openai").is_ok());
        for bad in [
            "http://router.example/openai",
            "https://user:pass@router.example/openai",
            "https://router.example/openai?key=secret",
            "https://router.example/openai#secret",
            "not-a-url",
        ] {
            assert!(validate_openai_base_url(bad).is_err(), "{bad}");
        }
    }

    #[tokio::test]
    async fn custom_openai_endpoint_streams_chat_completions() {
        async fn completion(
            headers: HeaderMap,
            Json(body): Json<Value>,
        ) -> ([(String, String); 1], String) {
            assert_eq!(headers.get("authorization").unwrap(), "Bearer test-key");
            assert_eq!(headers.get("user-agent").unwrap(), "chatflow/0.1");
            assert_eq!(body["model"], "gpt-4o-mini");
            assert_eq!(body["stream"], true);
            assert_eq!(body["stream_options"]["include_usage"], true);
            let chunks = [
                json!({"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"gpt-4o-mini","choices":[{"index":0,"delta":{"content":"hello"},"finish_reason":null}]}),
                json!({"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"gpt-4o-mini","choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
                json!({"id":"chatcmpl-1","object":"chat.completion.chunk","created":1,"model":"gpt-4o-mini","choices":[],"usage":{"prompt_tokens":1,"completion_tokens":1,"total_tokens":2}}),
            ];
            let mut sse = chunks
                .iter()
                .map(|chunk| format!("data: {chunk}\n\n"))
                .collect::<String>();
            sse.push_str("data: [DONE]\n\n");
            ([("content-type".into(), "text/event-stream".into())], sse)
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/openai", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/openai/chat/completions", post(completion)),
            )
            .await
            .unwrap();
        });
        let client = build_openai_client("test-key", &url).unwrap();
        let model = client.completion_model("gpt-4o-mini");
        let mut stream = model
            .completion_request("hello")
            .max_tokens(10)
            .stream()
            .await
            .unwrap();
        let mut content = String::new();
        let mut usage = None;
        while let Some(chunk) = stream.next().await {
            match chunk.unwrap() {
                StreamedAssistantContent::Text(part) => content.push_str(&part.text),
                StreamedAssistantContent::Final(final_response) => {
                    usage = Some(final_response.usage)
                }
                _ => {}
            }
        }
        assert_eq!(content, "hello");
        assert_eq!(usage.unwrap().output_tokens, 1);
        server.abort();
    }

    #[tokio::test]
    async fn vault_reads_only_api_key_from_kv_v2() {
        async fn secret(headers: HeaderMap) -> Json<Value> {
            assert_eq!(headers.get("x-vault-token").unwrap(), "test-token");
            Json(json!({"data":{"data":{"api_key":"test-key"},"metadata":{}}}))
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route("/v1/secret/data/tenant/openai", get(secret)),
            )
            .await
            .unwrap();
        });
        let vault = VaultClient::new(url, "test-token".into()).unwrap();
        assert_eq!(
            vault.read_api_key("tenant/openai").await.unwrap(),
            "test-key"
        );
        assert!(vault.read_api_key("../other").await.is_err());
        server.abort();
    }

    #[tokio::test]
    async fn router_resolves_only_tenant_owned_credentials() {
        let Ok(database_url) = std::env::var("DATABASE_URL") else {
            return;
        };
        let pool = PgPool::connect(&database_url).await.unwrap();
        sqlx::migrate!().run(&pool).await.unwrap();
        let owned: Uuid = sqlx::query_scalar("INSERT INTO tenants(name) VALUES ($1) RETURNING id")
            .bind(Uuid::new_v4().to_string())
            .fetch_one(&pool)
            .await
            .unwrap();
        let foreign: Uuid =
            sqlx::query_scalar("INSERT INTO tenants(name) VALUES ($1) RETURNING id")
                .bind(Uuid::new_v4().to_string())
                .fetch_one(&pool)
                .await
                .unwrap();
        let reference = format!("tenants/{owned}/openai");
        let id: Uuid = sqlx::query_scalar("INSERT INTO provider_credentials(tenant_id, provider_alias, secret_reference) VALUES ($1, 'openai', $2) RETURNING id")
            .bind(owned).bind(&reference).fetch_one(&pool).await.unwrap();
        async fn secret() -> Json<Value> {
            Json(json!({"data":{"data":{"api_key":"owned-key"}}}))
        }
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let server = tokio::spawn(async move {
            axum::serve(
                listener,
                Router::new().route(&format!("/v1/secret/data/{reference}"), get(secret)),
            )
            .await
            .unwrap();
        });
        let router = RouterProvider {
            pool,
            vault: Some(VaultClient::new(url, "token".into()).unwrap()),
            openai_model: None,
            openai_base_url: DEFAULT_OPENAI_BASE_URL.into(),
            mock: MockProvider,
        };
        assert_eq!(
            router.resolve_openai_key(owned, id).await.unwrap(),
            "owned-key"
        );
        assert!(router.resolve_openai_key(foreign, id).await.is_err());
        server.abort();
    }
}
