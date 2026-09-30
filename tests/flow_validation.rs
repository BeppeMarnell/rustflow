use chatflow::flow::{valid_variable_name, validate, FlowDefinition, FlowError};

fn example() -> FlowDefinition {
    serde_json::from_str(include_str!("../examples/basic-chat.json")).unwrap()
}

fn conditional() -> FlowDefinition {
    serde_json::from_str(include_str!("../examples/conditional-chat.json")).unwrap()
}

#[test]
fn example_validates() {
    validate(&example()).unwrap();
}

#[test]
fn variable_names_and_bindings_are_validated() {
    for name in ["memory", "A_2"] {
        assert!(valid_variable_name(name));
    }
    for name in ["", "with space", "a-b", &"x".repeat(65)] {
        assert!(!valid_variable_name(name));
    }
    let mut flow = example();
    flow.nodes[1].kind = "variable.set".into();
    flow.nodes[1].config = serde_json::json!({"name":"memory"});
    flow.nodes[1].bindings.clear();
    assert_eq!(
        validate(&flow),
        Err(FlowError::MissingInput("prompt".into(), "value".into()))
    );
    flow.nodes[1].bindings.insert(
        "value".into(),
        chatflow::flow::Binding {
            node: "message".into(),
            output: "value".into(),
        },
    );
    flow.edges[0].to_input = "value".into();
    validate(&flow).unwrap();
    flow.nodes[1].config = serde_json::json!({"name":"with space"});
    assert!(matches!(validate(&flow), Err(FlowError::Config(_, _))));
}

#[test]
fn rejects_invalid_prompt_byte_limit() {
    for limit in [0, 65_537] {
        let mut flow = example();
        flow.nodes
            .iter_mut()
            .find(|node| node.id == "model")
            .unwrap()
            .config["max_prompt_bytes"] = serde_json::json!(limit);
        assert!(matches!(validate(&flow), Err(FlowError::Config(_, _))));
    }
}

#[test]
fn conditional_example_validates() {
    validate(&conditional()).unwrap();
}

#[test]
fn rejects_invalid_condition_guard() {
    let mut flow = conditional();
    flow.nodes[2].run_if.as_mut().unwrap().node = "message".into();
    assert_eq!(
        validate(&flow),
        Err(FlowError::InvalidGuard("greeting".into()))
    );
}

#[test]
fn rejects_boolean_binding_to_string_port() {
    let mut flow = conditional();
    flow.nodes[2].bindings.get_mut("text").unwrap().node = "is_hello".into();
    assert!(matches!(
        validate(&flow),
        Err(FlowError::TypeMismatch(_, _))
    ));
}

#[test]
fn rejects_unknown_node_type() {
    let mut flow = example();
    flow.nodes[1].kind = "shell.exec".into();
    assert_eq!(
        validate(&flow),
        Err(FlowError::UnknownType("shell.exec".into()))
    );
}

#[test]
fn rejects_bad_binding() {
    let mut flow = example();
    flow.nodes[1].bindings.get_mut("text").unwrap().node = "missing".into();
    assert!(matches!(
        validate(&flow),
        Err(FlowError::UnknownSource(_, _))
    ));
}

#[test]
fn rejects_cycle() {
    let mut flow = example();
    flow.nodes[1].bindings.get_mut("text").unwrap().node = "reply".into();
    flow.edges[0].from.node = "reply".into();
    assert_eq!(validate(&flow), Err(FlowError::Cycle));
}

#[test]
fn rejects_missing_output() {
    let mut flow = example();
    flow.outputs.clear();
    assert_eq!(validate(&flow), Err(FlowError::MissingOutput));
}

#[test]
fn rejects_output_that_bypasses_output_node() {
    let mut flow = example();
    flow.outputs.get_mut("reply").unwrap().node = "model".into();
    assert_eq!(validate(&flow), Err(FlowError::MissingOutput));
}

#[test]
fn rejects_edge_disagreement() {
    let mut flow = example();
    flow.edges.pop();
    assert_eq!(validate(&flow), Err(FlowError::EdgeMismatch));
}

#[test]
fn checked_in_schema_matches_contracts() {
    let checked_in: serde_json::Value =
        serde_json::from_str(include_str!("../schemas/catalog.schema.json")).unwrap();
    let current = serde_json::json!({
        "flow": chatflow::flow::json_schema(),
        "node_catalog": chatflow::flow::node_catalog_schema(),
    });
    assert_eq!(checked_in, current);
}

#[test]
fn rag_example_validates() {
    let flow: FlowDefinition =
        serde_json::from_str(include_str!("../examples/rag-chat.json")).unwrap();
    chatflow::flow::validate(&flow).unwrap();
}

#[test]
fn rag_context_output_is_available_only_on_retrieval_nodes() {
    let mut flow: FlowDefinition =
        serde_json::from_str(include_str!("../examples/rag-chat.json")).unwrap();
    validate(&flow).unwrap();
    let prompt = flow
        .nodes
        .iter_mut()
        .find(|node| node.id == "prompt")
        .unwrap();
    prompt.bindings.get_mut("text").unwrap().output = "missing".into();
    assert_eq!(
        validate(&flow),
        Err(FlowError::UnknownOutput("sources".into(), "missing".into()))
    );
    let prompt = flow
        .nodes
        .iter_mut()
        .find(|node| node.id == "prompt")
        .unwrap();
    prompt.bindings.get_mut("text").unwrap().output = "context".into();
    prompt.bindings.get_mut("question").unwrap().output = "context".into();
    assert_eq!(
        validate(&flow),
        Err(FlowError::UnknownOutput("message".into(), "context".into()))
    );
}

#[test]
fn rag_prompt_requires_a_bound_question_when_template_uses_it() {
    let mut flow: FlowDefinition =
        serde_json::from_str(include_str!("../examples/rag-chat.json")).unwrap();
    flow.nodes
        .iter_mut()
        .find(|node| node.id == "prompt")
        .unwrap()
        .bindings
        .remove("question");
    assert_eq!(
        validate(&flow),
        Err(FlowError::MissingInput("prompt".into(), "question".into()))
    );
}

#[test]
fn openai_nodes_require_a_credential_id_and_known_alias() {
    let mut flow = example();
    flow.nodes[2].config["model_alias"] = serde_json::json!("openai");
    assert!(matches!(validate(&flow), Err(FlowError::Config(_, _))));
    flow.nodes[2].config["credential_id"] = serde_json::json!(uuid::Uuid::new_v4());
    validate(&flow).unwrap();
    flow.nodes[2].config["model_alias"] = serde_json::json!("custom_url");
    assert!(matches!(validate(&flow), Err(FlowError::Config(_, _))));
}
