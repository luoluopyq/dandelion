//! G5-H C11: `todo` builtin. List persists in SQLite across turns; compact reinjects as user segment.

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::provider::types::{ChatMessage, ToolCall};
use dandelion_core::store::Store;
use dandelion_core::tools::todo::INJECTION_MARK;
use dandelion_core::{PermissionMode, PromptRequest, Runtime, Settings};
use std::sync::Arc;

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

fn todo_call(id: &str, arguments: &str) -> ToolCall {
    ToolCall {
        id: id.into(),
        name: "todo".into(),
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

#[tokio::test]
async fn todo_persists_across_turns_and_runtime() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools(
            "",
            vec![todo_call(
                "c1",
                r#"{"todos":[{"id":"1","content":"Ship C11","status":"in_progress"},{"id":"2","content":"Write tests","status":"pending"}]}"#,
            )],
        ),
        ScriptedTurn::text("noted"),
        ScriptedTurn::tools("", vec![todo_call("c2", "{}")]),
        ScriptedTurn::text("still there"),
    ]));
    let rt = runtime_with(
        dir.path(),
        provider.clone(),
        Settings::defaults_configured(),
    );
    let sid = rt.create_session(Some("todo")).expect("session");
    rt.prompt(PromptRequest::new(sid.clone(), "plan the work"))
        .await
        .expect("prompt 1");
    let listed = rt.todo_list(&sid).expect("list");
    assert_eq!(listed.len(), 2, "{listed:?}");
    assert_eq!(listed[0].id, "1");
    assert_eq!(listed[0].status, "in_progress");

    rt.prompt(PromptRequest::new(sid.clone(), "what is left?"))
        .await
        .expect("prompt 2");
    let store = Store::open(&dir.path().join("data"), 5000).unwrap();
    let tools = tool_contents(&store, &sid);
    assert!(
        tools
            .iter()
            .any(|t| t.contains("Ship C11") && t.contains("revision")),
        "tool result must echo list: {tools:?}"
    );
    let last = tools.last().expect("second todo result");
    assert!(last.contains("Write tests"), "{last}");
    assert!(
        last.contains("\"total\":2") || last.contains("\"total\": 2"),
        "{last}"
    );

    drop(rt);
    let rt2 = runtime_with(
        dir.path(),
        Arc::new(ScriptedProvider::new(vec![])),
        Settings::defaults_configured(),
    );
    let again = rt2.todo_list(&sid).expect("reopen");
    assert_eq!(again.len(), 2);
    assert_eq!(again[1].content, "Write tests");
}

#[tokio::test]
async fn todo_schema_is_on_session_not_g1() {
    let dir = tempfile::tempdir().unwrap();
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    let rt = runtime_with(
        dir.path(),
        provider.clone(),
        Settings::defaults_configured(),
    );
    let sid = rt.create_session(Some("todo-schema")).unwrap();
    rt.prompt(PromptRequest::new(sid, "hi"))
        .await
        .expect("prompt");
    let names = provider.last_tool_names();
    assert!(names.contains(&"todo".to_string()), "{names:?}");
    assert!(names.contains(&"read_file".to_string()), "{names:?}");
}

#[tokio::test]
async fn compact_reinjects_todo_as_user_not_system() {
    let dir = tempfile::tempdir().unwrap();
    let mut settings = Settings::defaults_configured();
    settings.compact_keep_turns = 1;
    let provider = Arc::new(ScriptedProvider::new(vec![
        ScriptedTurn::tools(
            "",
            vec![todo_call(
                "c1",
                r#"{"todos":[{"id":"1","content":"UNIQUE_TODO_ALPHA","status":"pending"}]}"#,
            )],
        ),
        ScriptedTurn::text("planned"),
        ScriptedTurn::overflow(),
        ScriptedTurn::text("summary-of-plan"),
        ScriptedTurn::text("recovered"),
    ]));
    let rt = runtime_with(dir.path(), provider.clone(), settings);
    let sid = rt.create_session(Some("todo-compact")).unwrap();
    rt.prompt(PromptRequest::new(sid.clone(), "make a plan"))
        .await
        .expect("p1");
    rt.prompt(PromptRequest::new(sid.clone(), "continue"))
        .await
        .expect("p2");
    let system = provider.last_system().unwrap_or_default();
    assert!(
        !system.contains("UNIQUE_TODO_ALPHA"),
        "todo must not enter system: {system}"
    );
    assert!(!system.contains(INJECTION_MARK), "{system}");
    let store = Store::open(&dir.path().join("data"), 5000).unwrap();
    let msgs = store.load_openai_messages(&sid).unwrap();
    let users: Vec<String> = msgs
        .into_iter()
        .filter_map(|m| match m {
            ChatMessage::User { content, .. } => Some(content),
            _ => None,
        })
        .collect();
    assert!(
        users
            .iter()
            .any(|u| u.contains(INJECTION_MARK) && u.contains("UNIQUE_TODO_ALPHA")),
        "compact must keep todos as user segment: {users:?}"
    );
}
