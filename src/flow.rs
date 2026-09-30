use std::collections::{BTreeMap, HashMap, HashSet};

use petgraph::{algo::is_cyclic_directed, graph::DiGraph};
use schemars::{schema_for, JsonSchema};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use uuid::Uuid;

use crate::FLOW_SCHEMA_VERSION;

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FlowDefinition {
    pub schema_version: u32,
    pub flow_id: Uuid,
    pub inputs: BTreeMap<String, ValueType>,
    pub outputs: BTreeMap<String, Binding>,
    pub nodes: Vec<NodeDefinition>,
    pub edges: Vec<EdgeDefinition>,
    pub limits: FlowLimits,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ValueType {
    String,
    Boolean,
    Object,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct NodeDefinition {
    pub id: String,
    #[serde(rename = "type")]
    pub kind: String,
    pub config: Value,
    #[serde(default)]
    pub bindings: BTreeMap<String, Binding>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run_if: Option<ConditionGuard>,
    #[serde(default)]
    pub retry: RetryPolicy,
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConditionGuard {
    pub node: String,
    pub equals: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct Binding {
    pub node: String,
    pub output: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
pub struct EdgeDefinition {
    pub from: Binding,
    pub to_node: String,
    pub to_input: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct FlowLimits {
    #[serde(default = "default_max_nodes")]
    pub max_nodes: usize,
    #[serde(default = "default_max_run_ms")]
    pub max_run_ms: u64,
    #[serde(default = "default_max_parallel_nodes")]
    pub max_parallel_nodes: usize,
}

impl Default for FlowLimits {
    fn default() -> Self {
        Self {
            max_nodes: default_max_nodes(),
            max_run_ms: default_max_run_ms(),
            max_parallel_nodes: default_max_parallel_nodes(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct RetryPolicy {
    #[serde(default)]
    pub max_attempts: u8,
}

const fn default_timeout_ms() -> u64 {
    30_000
}
const fn default_max_nodes() -> usize {
    64
}
const fn default_max_run_ms() -> u64 {
    120_000
}
const fn default_max_parallel_nodes() -> usize {
    4
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum FlowError {
    #[error("unsupported schema version {0}")]
    SchemaVersion(u32),
    #[error("flow must declare at least one output")]
    MissingOutput,
    #[error("flow has too many nodes or an invalid node limit")]
    NodeLimit,
    #[error("flow has an invalid run time limit")]
    RunLimit,
    #[error("duplicate or empty node ID: {0}")]
    NodeId(String),
    #[error("unknown node type: {0}")]
    UnknownType(String),
    #[error("invalid configuration for node {0}: {1}")]
    Config(String, String),
    #[error("invalid node policy: {0}")]
    Policy(String),
    #[error("unknown input {1} on node {0}")]
    UnknownInput(String, String),
    #[error("missing input {1} on node {0}")]
    MissingInput(String, String),
    #[error("unknown binding source {1} on node {0}")]
    UnknownSource(String, String),
    #[error("unknown output {1} on node {0}")]
    UnknownOutput(String, String),
    #[error("incompatible binding from {0} to {1}")]
    TypeMismatch(String, String),
    #[error("a node cannot bind to itself: {0}")]
    SelfReference(String),
    #[error("invalid condition guard on node {0}")]
    InvalidGuard(String),
    #[error("declared edges do not match bindings")]
    EdgeMismatch,
    #[error("flow contains a cycle")]
    Cycle,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct InputConfig {
    name: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct PromptConfig {
    template: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ChatConfig {
    model_alias: String,
    #[serde(default)]
    credential_id: Option<Uuid>,
    max_output_tokens: u32,
    #[serde(default = "default_max_prompt_bytes")]
    max_prompt_bytes: u32,
}
pub const fn default_max_prompt_bytes() -> u32 {
    16_384
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct ConditionConfig {
    equals: String,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct SelectConfig {}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct OutputConfig {}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct RagConfig {
    top_k: u8,
}
#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
struct VariableConfig {
    name: String,
}

pub fn json_schema() -> Value {
    serde_json::to_value(schema_for!(FlowDefinition)).expect("schema serializes")
}

/// Schemas clients can use to build and inspect the reviewed node catalog.
pub fn node_catalog_schema() -> BTreeMap<&'static str, Value> {
    let mut catalog = BTreeMap::new();
    for (kind, config) in [
        (
            "input",
            serde_json::to_value(schema_for!(InputConfig)).expect("schema"),
        ),
        (
            "prompt.render",
            serde_json::to_value(schema_for!(PromptConfig)).expect("schema"),
        ),
        (
            "llm.chat",
            serde_json::to_value(schema_for!(ChatConfig)).expect("schema"),
        ),
        (
            "condition",
            serde_json::to_value(schema_for!(ConditionConfig)).expect("schema"),
        ),
        (
            "select",
            serde_json::to_value(schema_for!(SelectConfig)).expect("schema"),
        ),
        (
            "rag.retrieve",
            serde_json::to_value(schema_for!(RagConfig)).expect("schema"),
        ),
        (
            "variable.get",
            serde_json::to_value(schema_for!(VariableConfig)).expect("schema"),
        ),
        (
            "variable.set",
            serde_json::to_value(schema_for!(VariableConfig)).expect("schema"),
        ),
        (
            "output",
            serde_json::to_value(schema_for!(OutputConfig)).expect("schema"),
        ),
    ] {
        let (input, output) = crate::node::interface_schema(kind).expect("known node type");
        catalog.insert(
            kind,
            serde_json::json!({"config": config, "input": input, "output": output}),
        );
    }
    catalog
}

pub fn validate(flow: &FlowDefinition) -> Result<(), FlowError> {
    if flow.schema_version != FLOW_SCHEMA_VERSION {
        return Err(FlowError::SchemaVersion(flow.schema_version));
    }
    if flow.outputs.is_empty() {
        return Err(FlowError::MissingOutput);
    }
    if flow.limits.max_nodes == 0
        || flow.limits.max_nodes > 256
        || flow.nodes.len() > flow.limits.max_nodes
    {
        return Err(FlowError::NodeLimit);
    }
    if flow.limits.max_run_ms == 0 || flow.limits.max_run_ms > 3_600_000 {
        return Err(FlowError::RunLimit);
    }
    if flow.limits.max_parallel_nodes == 0 || flow.limits.max_parallel_nodes > 32 {
        return Err(FlowError::NodeLimit);
    }

    let mut by_id = HashMap::new();
    for (index, node) in flow.nodes.iter().enumerate() {
        if node.id.is_empty() || by_id.insert(node.id.as_str(), index).is_some() {
            return Err(FlowError::NodeId(node.id.clone()));
        }
    }
    let mut graph = DiGraph::<(), ()>::new();
    let indices: Vec<_> = flow.nodes.iter().map(|_| graph.add_node(())).collect();
    let mut expected_edges = HashSet::new();
    for node in &flow.nodes {
        let required = match node.kind.as_str() {
            "input" => {
                let config: InputConfig = parse_config(node)?;
                if !flow.inputs.contains_key(&config.name) {
                    return Err(FlowError::Config(
                        node.id.clone(),
                        format!("undeclared flow input: {}", config.name),
                    ));
                }
                if !matches!(flow.inputs.get(&config.name), Some(ValueType::String)) {
                    return Err(FlowError::TypeMismatch(config.name, node.id.clone()));
                }
                &[][..]
            }
            "prompt.render" => {
                let config: PromptConfig = parse_config(node)?;
                if config.template.is_empty() {
                    return Err(FlowError::Config(node.id.clone(), "empty template".into()));
                }
                if config.template.contains("{{question}}")
                    && !node.bindings.contains_key("question")
                {
                    return Err(FlowError::MissingInput(node.id.clone(), "question".into()));
                }
                &["text"][..]
            }
            "llm.chat" => {
                let config: ChatConfig = parse_config(node)?;
                if !matches!(config.model_alias.as_str(), "mock" | "openai")
                    || !(1..=4096).contains(&config.max_output_tokens)
                    || !(1..=65_536).contains(&config.max_prompt_bytes)
                    || (config.model_alias == "openai" && config.credential_id.is_none())
                    || (config.model_alias == "mock" && config.credential_id.is_some())
                {
                    return Err(FlowError::Config(
                        node.id.clone(),
                        "invalid model alias, credential ID, token limit, or prompt byte limit"
                            .into(),
                    ));
                }
                &["prompt"][..]
            }
            "condition" => {
                let config: ConditionConfig = parse_config(node)?;
                if config.equals.len() > 4_096 {
                    return Err(FlowError::Config(
                        node.id.clone(),
                        "condition value is too long".into(),
                    ));
                }
                &["text"][..]
            }
            "select" => {
                let _: SelectConfig = parse_config(node)?;
                &["condition", "when_true", "when_false"][..]
            }
            "rag.retrieve" => {
                let config: RagConfig = parse_config(node)?;
                if !(1..=10).contains(&config.top_k) {
                    return Err(FlowError::Config(
                        node.id.clone(),
                        "top_k must be 1 to 10".into(),
                    ));
                }
                &["query"][..]
            }
            "variable.get" | "variable.set" => {
                let config: VariableConfig = parse_config(node)?;
                if !valid_variable_name(&config.name) {
                    return Err(FlowError::Config(
                        node.id.clone(),
                        "invalid variable name".into(),
                    ));
                }
                if node.kind == "variable.set" {
                    &["value"][..]
                } else {
                    &[][..]
                }
            }
            "output" => {
                let _: OutputConfig = parse_config(node)?;
                &["value"][..]
            }
            _ => return Err(FlowError::UnknownType(node.kind.clone())),
        };
        if node.timeout_ms == 0
            || node.timeout_ms > flow.limits.max_run_ms
            || node.retry.max_attempts > 10
        {
            return Err(FlowError::Policy(node.id.clone()));
        }
        for port in required {
            if !node.bindings.contains_key(*port) {
                return Err(FlowError::MissingInput(node.id.clone(), (*port).into()));
            }
        }
        for (port, source) in &node.bindings {
            if !required.contains(&port.as_str())
                && !(node.kind == "prompt.render" && port == "question")
            {
                return Err(FlowError::UnknownInput(node.id.clone(), port.clone()));
            }
            check_source(flow, &by_id, source, &node.id)?;
            let source_node = &flow.nodes[by_id[source.node.as_str()]];
            let source_boolean = source_node.kind == "condition";
            let target_boolean = node.kind == "select" && port == "condition";
            if source_boolean != target_boolean {
                return Err(FlowError::TypeMismatch(
                    source.node.clone(),
                    node.id.clone(),
                ));
            }
            if source.node == node.id {
                return Err(FlowError::SelfReference(node.id.clone()));
            }
            graph.add_edge(
                indices[by_id[source.node.as_str()]],
                indices[by_id[node.id.as_str()]],
                (),
            );
            expected_edges.insert(EdgeDefinition {
                from: source.clone(),
                to_node: node.id.clone(),
                to_input: port.clone(),
            });
        }
        if let Some(guard) = &node.run_if {
            let Some(&source_index) = by_id.get(guard.node.as_str()) else {
                return Err(FlowError::InvalidGuard(node.id.clone()));
            };
            let source_node = &flow.nodes[source_index];
            if source_node.kind != "condition"
                || source_node.run_if.is_some()
                || guard.node == node.id
            {
                return Err(FlowError::InvalidGuard(node.id.clone()));
            }
            graph.add_edge(indices[source_index], indices[by_id[node.id.as_str()]], ());
        }
    }
    for source in flow.outputs.values() {
        check_source(flow, &by_id, source, "flow")?;
        if flow.nodes[by_id[source.node.as_str()]].kind != "output" {
            return Err(FlowError::MissingOutput);
        }
    }
    if expected_edges.len() != flow.edges.len()
        || flow.edges.iter().cloned().collect::<HashSet<_>>() != expected_edges
    {
        return Err(FlowError::EdgeMismatch);
    }
    if is_cyclic_directed(&graph) {
        return Err(FlowError::Cycle);
    }
    Ok(())
}

fn parse_config<T: for<'de> Deserialize<'de>>(node: &NodeDefinition) -> Result<T, FlowError> {
    serde_json::from_value(node.config.clone())
        .map_err(|error| FlowError::Config(node.id.clone(), error.to_string()))
}

fn check_source(
    flow: &FlowDefinition,
    by_id: &HashMap<&str, usize>,
    source: &Binding,
    target: &str,
) -> Result<(), FlowError> {
    let Some(&index) = by_id.get(source.node.as_str()) else {
        return Err(FlowError::UnknownSource(target.into(), source.node.clone()));
    };
    let node = &flow.nodes[index];
    if source.output != "value" && !(node.kind == "rag.retrieve" && source.output == "context") {
        return Err(FlowError::UnknownOutput(
            source.node.clone(),
            source.output.clone(),
        ));
    }
    if !matches!(
        node.kind.as_str(),
        "input"
            | "prompt.render"
            | "llm.chat"
            | "condition"
            | "select"
            | "rag.retrieve"
            | "variable.get"
            | "variable.set"
            | "output"
    ) {
        return Err(FlowError::UnknownType(node.kind.clone()));
    }
    Ok(())
}

pub fn valid_variable_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}
