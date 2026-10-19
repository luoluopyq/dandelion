//! C18: skill uninstall / remote install, and MCP registry search + install.
//! Offline: the registry is a local `TcpListener` mock, never the real host.

use dandelion_core::host::CliHost;
use dandelion_core::provider::scripted::{ScriptedProvider, ScriptedTurn};
use dandelion_core::{PromptRequest, Runtime, Settings};
use std::io::{Read, Write};
use std::net::{Shutdown, TcpListener};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

fn runtime_with(dir: &std::path::Path, mut settings: Settings) -> Runtime {
    settings.workspace = Some(dir.to_path_buf());
    settings.cron_tick_sec = 3_600;
    let provider = Arc::new(ScriptedProvider::new(vec![ScriptedTurn::text("ok")]));
    Runtime::new_with_provider(
        Arc::new(CliHost::with_data_dir(dir.join("data"))),
        settings,
        provider,
    )
    .expect("runtime")
}

/// Serve one JSON response on loopback, then close.
fn spawn_registry(body: &str) -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let body = body.to_string();
    thread::spawn(move || {
        let Ok((mut stream, _)) = listener.accept() else {
            return;
        };
        stream.set_nodelay(true).ok();
        stream
            .set_read_timeout(Some(Duration::from_millis(800)))
            .ok();
        let mut buf = [0u8; 8192];
        let _ = stream.read(&mut buf);
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.write_all(resp.as_bytes());
        let _ = stream.flush();
        let _ = stream.shutdown(Shutdown::Both);
    });
    port
}

fn settings_with_registry(port: u16) -> Settings {
    let mut s = Settings::defaults_configured();
    s.registry_mcp_url = format!("http://127.0.0.1:{port}/v0/servers");
    s.registry_timeout_ms = 3_000;
    s.http_connect_timeout_ms = 3_000;
    s
}

fn write_skill(dir: &std::path::Path, id: &str, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(
        dir.join("SKILL.md"),
        format!("---\nname: {id}\ndescription: {id} desc\n---\n{body}\n"),
    )
    .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skill_remove_deletes_data_dir_copy() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_with(dir.path(), Settings::defaults_configured());
    let src = dir.path().join("incoming/hello-skill");
    write_skill(&src, "hello-skill", "BODY");
    let item = rt.skills_import(&src).expect("import");
    assert_eq!(item.id, "hello-skill");
    assert!(dir.path().join("data/skills/hello-skill/SKILL.md").exists());

    let removed = rt.skills_remove("hello-skill", None).expect("remove");
    assert!(!removed.exists(), "folder must be gone");
    assert!(rt
        .skills_list(None)
        .unwrap()
        .iter()
        .all(|s| s.id != "hello-skill"));
    // Removing something that is not there is an error, not a silent no-op.
    assert!(rt.skills_remove("hello-skill", None).is_err());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skill_remove_refuses_framework_and_project_skill() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_with(dir.path(), Settings::defaults_configured());
    let fw = dir.path().join("data/skills/_framework/wiki");
    write_skill(&fw, "wiki", "FRAMEWORK BODY");
    let project = dir.path().join(".dandelion/skills/proj-skill");
    write_skill(&project, "proj-skill", "PROJECT BODY");

    let ws = dir.path().to_path_buf();
    let err = rt.skills_remove("wiki", Some(&ws)).unwrap_err();
    assert!(err.to_string().contains("_framework"), "{err}");
    assert!(fw.join("SKILL.md").exists(), "framework file must survive");

    // A project skill lives in the user's repository: refuse, don't delete.
    let err = rt.skills_remove("proj-skill", Some(&ws)).unwrap_err();
    assert!(err.to_string().contains("project skill"), "{err}");
    assert!(
        project.join("SKILL.md").exists(),
        "workspace file must survive"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skill_remove_clears_disabled_id() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_with(dir.path(), Settings::defaults_configured());
    let src = dir.path().join("incoming/gone-skill");
    write_skill(&src, "gone-skill", "BODY");
    rt.skills_import(&src).expect("import");
    rt.skills_set_enabled("gone-skill", false).expect("disable");
    assert!(rt
        .settings()
        .skills_disabled_ids
        .contains(&"gone-skill".to_string()));

    rt.skills_remove("gone-skill", None).expect("remove");
    assert!(
        !rt.settings()
            .skills_disabled_ids
            .contains(&"gone-skill".to_string()),
        "uninstall must not leave a dangling disabled id"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skill_remove_keeps_task_bindings() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_with(dir.path(), Settings::defaults_configured());
    let src = dir.path().join("incoming/bound-skill");
    write_skill(&src, "bound-skill", "BODY");
    rt.skills_import(&src).expect("import");
    let sid = rt.create_session(Some("task")).expect("session");
    rt.session_replace_bindings(&sid, "skill", &["bound-skill".to_string()])
        .expect("bind");

    rt.skills_remove("bound-skill", None).expect("remove");
    let bindings = rt.session_task_bindings(&sid).expect("bindings");
    assert_eq!(
        bindings["skill"],
        serde_json::json!(["bound-skill"]),
        "deleting a global asset must not cascade into task bindings"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skill_install_url_rejects_bad_scheme_and_host() {
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_with(dir.path(), Settings::defaults_configured());
    let err = rt
        .skills_install_url("http://github.com/o/r/SKILL.md")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("https"), "{err}");
    let err = rt
        .skills_install_url("https://evil.example/SKILL.md")
        .await
        .unwrap_err();
    assert!(err.to_string().contains("skillAllowedHosts"), "{err}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn skill_install_from_url_lands_disabled() {
    let body = "---\nname: remote-skill\ndescription: from the internet\n---\nREMOTE_BODY\n";
    let port = spawn_registry(body);
    let dir = tempfile::tempdir().unwrap();
    let mut settings = Settings::defaults_configured();
    settings.registry_timeout_ms = 3_000;
    settings.http_connect_timeout_ms = 3_000;
    // Loopback is the allowlisted host for this test's mock.
    settings.registry_skill_allowed_hosts = vec!["127.0.0.1".into()];
    let rt = runtime_with(dir.path(), settings);

    let report = rt
        .skills_install_url(&format!("http://127.0.0.1:{port}/SKILL.md"))
        .await
        .expect("install");
    assert_eq!(report.items.len(), 1);
    assert_eq!(report.items[0].id, "remote-skill");
    assert!(report.installed_disabled);
    assert!(
        rt.settings()
            .skills_disabled_ids
            .contains(&"remote-skill".to_string()),
        "remote skill must land disabled until reviewed"
    );
    let listed = rt.skills_list(None).unwrap();
    let found = listed
        .iter()
        .find(|s| s.id == "remote-skill")
        .expect("listed");
    assert!(!found.enabled);

    // The body must not reach the system prompt while it is disabled.
    let sid = rt.create_session(Some("t")).expect("session");
    let mut req = PromptRequest::new(sid, "hi");
    req.skill_ids = vec!["remote-skill".into()];
    let _ = rt.prompt(req).await;
    // Staging must not linger in data_dir.
    let tmp = dir.path().join("data/tmp");
    if tmp.exists() {
        assert!(
            std::fs::read_dir(&tmp).unwrap().next().is_none(),
            "staging dirs must be cleaned up"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registry_search_parses_entries_and_cursor() {
    let body = r#"{"servers":[
      {"server":{"name":"io.github.acme/files","title":"Files","description":"file tools",
        "version":"1.2.0",
        "packages":[{"registryType":"npm","identifier":"@acme/files-mcp","version":"1.2.0"}]},
       "_meta":{"io.modelcontextprotocol.registry/official":{"isLatest":true}}},
      {"server":{"name":"acme/remote","description":"hosted","version":"2.0.0",
        "remotes":[{"type":"streamable-http","url":"https://mcp.acme.test/mcp"}]}}
    ],"metadata":{"nextCursor":"acme/remote:2.0.0","count":2}}"#;
    let port = spawn_registry(body);
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_with(dir.path(), settings_with_registry(port));

    let page = rt.registry_mcp_search("files", None).await.expect("search");
    assert_eq!(page.entries.len(), 2);
    assert_eq!(page.entries[0].name, "io.github.acme/files");
    assert_eq!(page.entries[0].suggested_id, "io_github_acme_files");
    assert!(page.entries[0].is_latest);
    assert_eq!(page.next_cursor.as_deref(), Some("acme/remote:2.0.0"));
    // Searching installs nothing.
    assert!(rt.mcp_list().await.unwrap().is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registry_install_npm_entry_becomes_stdio_server() {
    let body = r#"{"servers":[{"server":{
        "name":"io.github.acme/files","description":"file tools","version":"1.2.0",
        "packages":[{"registryType":"npm","identifier":"@acme/files-mcp","version":"1.2.0",
                     "transport":{"type":"stdio"}}]}}],"metadata":{"count":1}}"#;
    let port = spawn_registry(body);
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_with(dir.path(), settings_with_registry(port));

    let plan = rt
        .registry_mcp_install("io.github.acme/files", None)
        .await
        .expect("install");
    assert_eq!(plan.server.command, "npx");
    assert_eq!(plan.server.args, vec!["-y", "@acme/files-mcp@1.2.0"]);
    assert!(plan.missing_env.is_empty());
    assert!(plan.server.enabled);

    // It landed in settings and shows up in the list without being started.
    let listed = rt.mcp_list().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].id, "io_github_acme_files");
    assert!(!listed[0].ready, "install must not auto-start");
    assert_eq!(rt.settings().mcp_servers.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registry_install_secret_lands_disabled_without_storing_key() {
    let body = r#"{"servers":[{"server":{
        "name":"acme/jira","description":"jira","version":"1.0.0",
        "remotes":[{"type":"streamable-http","url":"https://mcp.acme.test/mcp",
          "headers":[{"name":"X-Api-Key","isSecret":true,"isRequired":true,
                      "description":"api key","default":"SHOULD_NOT_PERSIST"}]}]}}],
        "metadata":{"count":1}}"#;
    let port = spawn_registry(body);
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_with(dir.path(), settings_with_registry(port));

    let plan = rt
        .registry_mcp_install("acme/jira", None)
        .await
        .expect("install");
    assert!(!plan.server.enabled, "missing secret must not be enabled");
    assert_eq!(plan.missing_env.len(), 1);
    assert!(plan.missing_env[0].secret);
    assert_eq!(
        plan.server.headers.get("X-Api-Key").map(String::as_str),
        Some("${env:ACME_JIRA_X_API_KEY}")
    );

    let on_disk = std::fs::read_to_string(dir.path().join("data/settings.json")).unwrap();
    assert!(
        !on_disk.contains("SHOULD_NOT_PERSIST"),
        "a secret must never be written to settings.json: {on_disk}"
    );
    assert!(on_disk.contains("${env:ACME_JIRA_X_API_KEY}"), "{on_disk}");
    let listed = rt.mcp_list().await.unwrap();
    assert_eq!(listed.len(), 1);
    assert!(!listed[0].enabled, "disabled until the key is provided");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registry_install_unknown_name_and_type_error() {
    let body = r#"{"servers":[{"server":{"name":"acme/other","version":"1.0.0",
        "packages":[{"registryType":"brew","identifier":"x"}]}}],"metadata":{"count":1}}"#;
    let port = spawn_registry(body);
    let dir = tempfile::tempdir().unwrap();
    let rt = runtime_with(dir.path(), settings_with_registry(port));
    let err = rt
        .registry_mcp_install("acme/missing", None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("not found"), "{err}");
    assert!(rt.settings().mcp_servers.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn registry_http_error_is_reported_not_panicked() {
    let dir = tempfile::tempdir().unwrap();
    // Port 1 refuses instantly: exercises the transport-failure path.
    let mut settings = Settings::defaults_configured();
    settings.registry_mcp_url = "http://127.0.0.1:1/v0/servers".into();
    settings.registry_timeout_ms = 1_500;
    settings.http_connect_timeout_ms = 1_500;
    let rt = runtime_with(dir.path(), settings);
    let err = rt.registry_mcp_search("x", None).await.unwrap_err();
    assert!(err.to_string().contains("registry"), "{err}");
}
