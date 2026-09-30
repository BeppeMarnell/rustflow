fn main() {
    let schema = serde_json::json!({
        "flow": chatflow::flow::json_schema(),
        "node_catalog": chatflow::flow::node_catalog_schema(),
    });
    println!(
        "{}",
        serde_json::to_string_pretty(&schema).expect("schema serializes")
    );
}
