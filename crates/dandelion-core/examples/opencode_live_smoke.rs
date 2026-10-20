//! 真机冒烟：OpenCode 引擎「每轮直启 run + 原生会话 ID 续接」。
//!
//! 用真实 settings.json、真实 opencode.exe 与真实模型服务连跑两轮：
//! 第一轮告知一个数字，第二轮要求其复述 —— 复述成功即证明跨进程
//! `--session` 续接生效。手动运行（不进步骤测试）：
//! `cargo run -p dandelion-core --example opencode_live_smoke`

use dandelion_core::agent_plugins::{self, AgentSessions, Engine};
use dandelion_core::config::{PermissionMode, Settings, SettingsOverlay};

#[tokio::main]
async fn main() -> dandelion_core::Result<()> {
    let data_dir = std::env::var("DLN_DATA_DIR").map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::path::PathBuf::from(
            "C:/Users/admin/AppData/Roaming/dandelion/dandelion/data"));
    let workspace = std::env::current_dir().unwrap_or_else(|_| std::env::temp_dir());
    let settings = Settings::load(&data_dir, &SettingsOverlay::default())?;
    let base_url = settings.base_url.clone()
        .ok_or_else(|| dandelion_core::Error::Settings("settings 缺 baseUrl".into()))?;
    let model_id = settings.default_model.clone()
        .ok_or_else(|| dandelion_core::Error::Settings("settings 缺 defaultModel".into()))?;
    let key = settings.api_key.clone().unwrap_or_else(|| "local".into());
    let executable = agent_plugins::executable(&data_dir, Engine::OpenCode, &[workspace.clone()])?;
    println!("exe: {}", executable.display());
    let config = serde_json::json!({"provider":{"dandelion":{
        "npm":"@ai-sdk/openai-compatible","name":"Dandelion",
        "options":{"baseURL":base_url,"apiKey":key},
        "models":{model_id.clone():{}}
    }}});
    let env = vec![(
        "OPENCODE_CONFIG_CONTENT".to_string(),
        serde_json::to_string(&config)?,
    )];
    let model = Some(format!("dandelion/{model_id}"));
    // 真机默认路径：permissionMode=ask 且 OpenCode 无审批通道 → 降级 Deny。
    let permission_mode = if settings.permission_mode == PermissionMode::Ask {
        PermissionMode::Deny
    } else {
        settings.permission_mode
    };
    println!("model: {model:?}  permission: {permission_mode:?}");

    let sessions = AgentSessions::default();
    for (turn, text) in [
        "你好。请记住这个数字：4271。只需要回复「记住了」。",
        "我刚才让你记住的数字是多少？只回复那个数字。",
    ].iter().enumerate() {
        println!("--- 第 {} 轮 ---", turn + 1);
        let mut printed = false;
        let answer = sessions.execute(
            Engine::OpenCode, "smoke", &executable, model.as_deref(), &workspace,
            text, text, &env, permission_mode,
            &[], None, &[], None, tokio_util::sync::CancellationToken::new(),
            |delta| { print!("{delta}"); printed = true; },
            |tool| println!("[tool {} {}]", if tool.finished { "end" } else { "start" }, tool.name),
        ).await?;
        if !printed { println!("{answer}"); }
        println!();
        if turn == 1 && !answer.contains("4271") {
            println!("!! 续接可疑：第二轮回答未包含 4271：{answer:?}");
        }
    }
    println!("session_ids: {:?}", sessions.session_ids(Engine::OpenCode));
    sessions.close("smoke");
    Ok(())
}
