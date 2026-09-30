use std::{future::Future, pin::Pin};

use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct NodeContext {
    pub tenant_id: Uuid,
    pub run_id: Uuid,
    pub trace_id: String,
}

#[derive(Debug, Error)]
pub enum NodeError {
    #[error("invalid node input: {0}")]
    InvalidInput(String),
    #[error("node execution failed: {0}")]
    Execution(String),
    #[error("retryable node failure: {0}")]
    Retryable(String),
}

pub type NodeFuture<'a> = Pin<Box<dyn Future<Output = Result<Value, NodeError>> + Send + 'a>>;

/// Implementations own their executable code; a flow can only refer to registered types.
pub trait NodeHandler: Send + Sync {
    fn kind(&self) -> &'static str;
    fn config_schema(&self) -> Value;
    fn input_schema(&self) -> Value;
    fn output_schema(&self) -> Value;
    fn execute<'a>(
        &'a self,
        context: &'a NodeContext,
        config: Value,
        input: Value,
    ) -> NodeFuture<'a>;
}

#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TextValue {
    pub value: String,
}

#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptInput {
    pub text: String,
    #[serde(default)]
    pub question: Option<String>,
}

#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConditionInput {
    pub text: String,
}

#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ChatInput {
    pub prompt: String,
}

#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OutputInput {
    pub value: String,
}

#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BooleanValue {
    pub value: bool,
}

#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectInput {
    pub condition: bool,
    pub when_true: Option<String>,
    pub when_false: Option<String>,
}

#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmptyInput {}

#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RagInput {
    pub query: String,
}

#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RagSource {
    pub dataset_id: String,
    pub document_id: String,
    pub chunk_id: String,
}
#[derive(JsonSchema, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RagValue {
    pub value: String,
    pub context: String,
    pub sources: Vec<RagSource>,
}

pub fn interface_schema(kind: &str) -> Option<(Value, Value)> {
    let schemas = match kind {
        "input" => (schema_for!(EmptyInput), schema_for!(TextValue)),
        "prompt.render" => (schema_for!(PromptInput), schema_for!(TextValue)),
        "llm.chat" => (schema_for!(ChatInput), schema_for!(TextValue)),
        "condition" => (schema_for!(ConditionInput), schema_for!(BooleanValue)),
        "select" => (schema_for!(SelectInput), schema_for!(TextValue)),
        "output" => (schema_for!(OutputInput), schema_for!(TextValue)),
        "rag.retrieve" => (schema_for!(RagInput), schema_for!(RagValue)),
        "variable.get" => (schema_for!(EmptyInput), schema_for!(TextValue)),
        "variable.set" => (schema_for!(OutputInput), schema_for!(TextValue)),
        _ => return None,
    };
    Some((
        serde_json::to_value(schemas.0).ok()?,
        serde_json::to_value(schemas.1).ok()?,
    ))
}
