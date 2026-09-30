use std::sync::Mutex;

use chatflow::provider::{
    ChatMessage, ChatRequest, ChatRole, MockProvider, ModelProvider, SinkFuture, TokenSink,
};

#[derive(Default)]
struct Collector(Mutex<Vec<String>>);

impl TokenSink for Collector {
    fn emit<'a>(&'a self, text: String) -> SinkFuture<'a> {
        Box::pin(async move {
            self.0.lock().unwrap().push(text);
            Ok(())
        })
    }
}

#[tokio::test]
async fn mock_streams_the_same_text_it_returns() {
    let sink = Collector::default();
    let response = MockProvider
        .chat(
            ChatRequest {
                tenant_id: uuid::Uuid::nil(),
                model_alias: "mock".into(),
                credential_id: None,
                messages: vec![ChatMessage {
                    role: ChatRole::User,
                    content: "hello world".into(),
                }],
                max_output_tokens: 100,
            },
            &sink,
        )
        .await
        .unwrap();
    assert_eq!(response.content, sink.0.lock().unwrap().join(""));
    assert!(sink.0.lock().unwrap().len() > 1);
}

#[tokio::test]
async fn mock_respects_output_token_limit() {
    let sink = Collector::default();
    let response = MockProvider
        .chat(
            ChatRequest {
                tenant_id: uuid::Uuid::nil(),
                model_alias: "mock".into(),
                credential_id: None,
                messages: vec![ChatMessage {
                    role: ChatRole::User,
                    content: "one two three four".into(),
                }],
                max_output_tokens: 3,
            },
            &sink,
        )
        .await
        .unwrap();
    assert_eq!(response.output_tokens, Some(3));
    assert_eq!(response.content, "Mock reply to:");
    assert_eq!(response.content, sink.0.lock().unwrap().join(""));
}
