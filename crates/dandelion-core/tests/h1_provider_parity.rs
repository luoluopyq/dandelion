//! H1a: Chat Completions and Responses must drive the same production loop.

use dandelion_core::host::CliHost;
use dandelion_core::provider::types::ChatMessage;
use dandelion_core::provider::{OpenAiResponsesSse, OpenAiSse, Provider};
use dandelion_core::store::Store;
use dandelion_core::{PermissionMode, PromptRequest, Runtime, Settings};
use std::sync::Arc;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn configured(server: &MockServer) -> Settings {
    let mut settings = Settings::defaults_configured();
    settings.base_url = Some(format!("{}/v1", server.uri()));
    settings.http_connect_timeout_ms = 5_000;
    settings.sse_idle_timeout_ms = 5_000;
    settings
}

async fn run(root: &std::path::Path, provider: Arc<dyn Provider>) -> (Vec<String>, Vec<String>) {
    let host = Arc::new(CliHost::with_data_dir(root.join("data")));
    let mut settings = Settings::defaults_configured();
    settings.workspace = Some(root.to_path_buf());
    settings.permission_mode = PermissionMode::Auto;
    let runtime = Runtime::new_with_provider(host, settings, provider).unwrap();
    let session = runtime.new_session(None);
    runtime
        .prompt(PromptRequest::new(session.clone(), "create parity.txt"))
        .await
        .unwrap();
    assert_eq!(
        std::fs::read_to_string(root.join("parity.txt")).unwrap(),
        "same\n"
    );

    let store = Store::open(&runtime.info().data_dir, 5_000).unwrap();
    let tool_ids = store
        .load_openai_messages(&session)
        .unwrap()
        .into_iter()
        .filter_map(|message| match message {
            ChatMessage::Tool { tool_call_id, .. } => Some(tool_call_id),
            _ => None,
        })
        .collect();
    let trace = runtime
        .trace_events(&session.to_string(), 100)
        .unwrap()
        .into_iter()
        .map(|event| event.kind)
        .collect();
    (tool_ids, trace)
}

#[tokio::test]
async fn chat_and_responses_have_identical_tool_side_effects_and_trace() {
    let chat_server = MockServer::start().await;
    let responses_server = MockServer::start().await;

    let chat_tool = r#"data: {"choices":[{"delta":{"tool_calls":[{"index":0,"id":"call_parity","function":{"name":"write_file","arguments":"{\"path\":\"parity.txt\",\"content\":\"same\\n\"}"}}]},"finish_reason":"tool_calls"}]}

data: [DONE]

"#;
    let chat_done = r#"data: {"choices":[{"delta":{"content":"done"},"finish_reason":"stop"}]}

data: [DONE]

"#;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string(chat_tool))
        .up_to_n_times(1)
        .mount(&chat_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/chat/completions"))
        .respond_with(ResponseTemplate::new(200).set_body_string(chat_done))
        .mount(&chat_server)
        .await;

    let responses_tool = r#"data: {"type":"response.function_call_arguments.done","call_id":"call_parity","name":"write_file","arguments":"{\"path\":\"parity.txt\",\"content\":\"same\\n\"}"}

data: {"type":"response.completed"}

"#;
    let responses_done = r#"data: {"type":"response.output_text.delta","delta":"done"}

data: {"type":"response.completed"}

"#;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_string(responses_tool))
        .up_to_n_times(1)
        .mount(&responses_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/responses"))
        .respond_with(ResponseTemplate::new(200).set_body_string(responses_done))
        .mount(&responses_server)
        .await;

    let chat_settings = configured(&chat_server);
    let responses_settings = configured(&responses_server);
    let chat_root = tempfile::tempdir().unwrap();
    let responses_root = tempfile::tempdir().unwrap();
    let chat = run(
        chat_root.path(),
        Arc::new(OpenAiSse::from_settings(&chat_settings).unwrap()),
    )
    .await;
    let responses = run(
        responses_root.path(),
        Arc::new(OpenAiResponsesSse::from_settings(&responses_settings).unwrap()),
    )
    .await;

    assert_eq!(chat.0, vec!["call_parity"]);
    assert_eq!(chat, responses);
}
