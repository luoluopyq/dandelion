//! G5-H C12: `web_search` / `web_extract`. Local HTTP mock only — never hit DuckDuckGo.

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::provider::types::{ChatMessage, ToolCall};
use dandelion_core::store::Store;
use dandelion_core::tools::ToolRegistry;
use dandelion_core::{PermissionMode, PromptRequest, Runtime, Settings};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

fn runtime_with(
    dir: &std::path::Path,
    provider: Arc<ScriptedProvider>,
    mut settings: Settings,
) -> Runtime {
    settings.workspace = Some(dir.to_path_buf());
    settings.permission_mode = PermissionMode::Auto;
    settings.cron_tick_sec = 3_600;
    Runtime::new_with_provider(
        Arc::new(CliHost::with_data_dir(dir.join("data"))),
        settings,
        provider,
    )
    .expect("runtime")
}

fn web_call(id: &str, name: &str, arguments: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: arguments.into(),
    }
}

fn tool_contents(store: &Store, sid: &dandelion_core::SessionId) -> Vec<String> {
    store
        .load_openai_messages(sid)
        .unwrap()
        .into_iter()
        .filter_map(|m| match m {
            ChatMessage::Tool { content, .. } => Some(content),
            _ => None,
        })
        .collect()
}

fn spawn_http_once(status: u16, content_type: &str, body: &str) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let body = body.to_string();
    let ct = content_type.to_string();
    thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        stream.set_nodelay(true).ok();
        stream
            .set_read_timeout(Some(Duration::from_millis(800)))
            .ok();
        let mut buf = [0u8; 4096];
        let _ = stream.read(&mut buf);
        let resp = format!(
            "HTTP/1.1 {status} OK\r\nContent-Type: {ct}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
        let _ = stream.flush();
        let _ = stream.shutdown(Shutdown::Both);
    });
    port
}

fn settings_for_local_web(search_url: Option<String>) -> Settings {
    let mut settings = Settings::defaults_configured();
    settings.web_timeout_ms = 3_000;
    settings.http_connect_timeout_ms = 3_000;
    if let Some(url) = search_url {
        settings.web_search_url = url;
    }
    settings
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn web_search_parses_json_from_local_mock() {
    let body = r#"{"results":[{"title":"SEARCH_HIT_TITLE","url":"https://example.com/hit","content":"snippet"}]}"#;
    let port = spawn_http_once(200, "application/json", body);
    let dir = tempfile::tempdir().unwrap();
    let settings =
        settings_for_local_web(Some(format!("http://127.0.0.1:{port}/search?q={{query}}")));
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools(
            "",
            vec![web_call("c1", "web_search", r#"{"query":"dandelion"}"#)],
        ),
        ScriptedTurn::text("found it"),
    ]));
    let rt = runtime_with(dir.path(), provider, settings);
    let sid = rt.create_session(Some("web-search")).expect("session");
    rt.prompt(PromptRequest::new(sid.clone(), "search"))
        .await
        .expect("prompt");
    let store = Store::open(&dir.path().join("data"), 5000).unwrap();
    let tools = tool_contents(&store, &sid);
    assert!(
        tools
            .iter()
            .any(|t| t.contains("SEARCH_HIT_TITLE") && t.contains("https://example.com/hit")),
        "{tools:?}"
    );
    assert!(tools.iter().all(|t| !t.starts_with("Error:")), "{tools:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn web_extract_strips_html_from_local_mock() {
    let html = "<html><script>evil()</script><p>EXTRACT_HIT_BODY &amp; more</p></html>";
    let port = spawn_http_once(200, "text/html", html);
    let dir = tempfile::tempdir().unwrap();
    let settings = settings_for_local_web(None);
    let args = format!(r#"{{"urls":["http://127.0.0.1:{port}/page"]}}"#);
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools("", vec![web_call("c1", "web_extract", &args)]),
        ScriptedTurn::text("read it"),
    ]));
    let rt = runtime_with(dir.path(), provider, settings);
    let sid = rt.create_session(Some("web-extract")).expect("session");
    rt.prompt(PromptRequest::new(sid.clone(), "extract"))
        .await
        .expect("prompt");
    let store = Store::open(&dir.path().join("data"), 5000).unwrap();
    let tools = tool_contents(&store, &sid);
    let last = tools.last().expect("extract result");
    assert!(last.contains("EXTRACT_HIT_BODY"), "{last}");
    assert!(last.contains("& more"), "{last}");
    assert!(!last.contains("evil()"), "{last}");
    assert!(!last.starts_with("Error:"), "{last}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn web_search_unreachable_is_error_and_loop_continues() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let dir = tempfile::tempdir().unwrap();
    let mut settings = Settings::defaults_configured();
    settings.web_search_url = format!("http://127.0.0.1:{port}/search?q={{query}}");
    settings.web_timeout_ms = 800;
    settings.http_connect_timeout_ms = 800;
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools(
            "",
            vec![web_call("c1", "web_search", r#"{"query":"offline"}"#)],
        ),
        ScriptedTurn::text("still idle"),
    ]));
    let rt = runtime_with(dir.path(), provider, settings);
    let sid = rt.create_session(Some("web-fail")).expect("session");
    rt.prompt(PromptRequest::new(sid.clone(), "search"))
        .await
        .expect("loop must not crash");
    let store = Store::open(&dir.path().join("data"), 5000).unwrap();
    let tools = tool_contents(&store, &sid);
    assert!(
        tools.iter().any(|t| t.starts_with("Error:")),
        "failure must be Error: {tools:?}"
    );
    let msgs = store.load_openai_messages(&sid).unwrap();
    assert!(
        msgs.iter().any(|m| matches!(
            m,
            ChatMessage::Assistant {
                content: Some(c),
                ..
            } if c.contains("still idle")
        )),
        "{msgs:?}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn web_extract_refuses_file_url() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools(
            "",
            vec![web_call(
                "c1",
                "web_extract",
                r#"{"urls":["file:///etc/passwd"]}"#,
            )],
        ),
        ScriptedTurn::text("blocked"),
    ]));
    let rt = runtime_with(dir.path(), provider, Settings::defaults_configured());
    let sid = rt.create_session(Some("web-file")).expect("session");
    rt.prompt(PromptRequest::new(sid.clone(), "extract"))
        .await
        .expect("prompt");
    let store = Store::open(&dir.path().join("data"), 5000).unwrap();
    let last = tool_contents(&store, &sid).pop().expect("result");
    assert!(last.contains("only http(s)"), "{last}");
    assert!(
        last.contains("file:///etc/passwd") || last.contains("error"),
        "{last}"
    );
}

#[test]
fn web_schema_is_on_session_not_g1() {
    let s = Settings::defaults();
    let g1 = ToolRegistry::g1(&s).names();
    assert_eq!(g1.len(), 6);
    assert!(!g1.contains(&"web_search".to_string()));
    assert!(!g1.contains(&"web_extract".to_string()));
    let session = ToolRegistry::session_tools(&s, true, false, vec![]).names();
    assert!(session.contains(&"web_search".to_string()), "{session:?}");
    assert!(session.contains(&"web_extract".to_string()), "{session:?}");
    assert!(session.contains(&"todo".to_string()), "{session:?}");
}
