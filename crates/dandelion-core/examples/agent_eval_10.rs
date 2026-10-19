//! Reproducible, isolated ten-task evaluation against a local MLX model.
//! Run with DANDELION_TEST_MLX_MODEL and DANDELION_MLX_SERVER set.

use dandelion_core::host::CliHost;
use dandelion_core::{
    MemoryKind, PermissionMode, PromptRequest, Runtime, Settings, TaskMode, ThinkingIntensity,
};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

struct Task {
    id: &'static str,
    prompt: &'static str,
    code: bool,
    corpus: bool,
}

fn tasks() -> Vec<Task> {
    vec![
        Task { id: "C1", code: true, corpus: false, prompt: "先用 read_file 读取 input.py，再用 write_file 创建 summary.md，用一句中文准确说明函数 add 的作用。必须实际读写文件。" },
        Task { id: "C2", code: true, corpus: false, prompt: "先读取 config.json，再把 mode 从 draft 改为 ready 并写回 config.json。必须实际读写文件。" },
        Task { id: "C3", code: true, corpus: false, prompt: "先读取 notes.txt，再把其中 TODO: 校对 改为 DONE: 校对。必须实际修改文件。" },
        Task { id: "C4", code: true, corpus: false, prompt: "读取 a.txt 和 b.txt，然后创建 report.txt，仅写出两个文件共有的代号。必须实际读取两个文件并写入报告。" },
        Task { id: "C5", code: true, corpus: false, prompt: "先读取 src/app.js，其中 multiply 错用了加法；请修复为乘法并写回。必须实际读写文件。" },
        Task { id: "D1", code: false, corpus: false, prompt: "根据你记住的用户偏好，回答我喜欢什么语言和答复长度，并说出记忆中的测试口令。" },
        Task { id: "D2", code: false, corpus: true, prompt: "根据绑定的知识资料回答：项目晨星的发布日期和负责人分别是什么？不要猜测。" },
        Task { id: "D3", code: false, corpus: true, prompt: "结合你记住的用户偏好与绑定的资料，给我周末活动建议，写明预算、地点、人数，遵守清单项数限制。" },
        Task { id: "D4", code: false, corpus: false, prompt: "用中文把买菜、运动、阅读整理成一份三项的今天清单。" },
        Task { id: "D5", code: false, corpus: false, prompt: "六个人平分三百六十元，每人多少元？只给出计算结果和一句解释。" },
    ]
}

fn seed_workspace(path: &Path, id: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    match id {
        "C1" => std::fs::write(path.join("input.py"), "def add(a, b):\n    return a + b\n")?,
        "C2" => std::fs::write(
            path.join("config.json"),
            "{\"mode\":\"draft\",\"retries\":2}\n",
        )?,
        "C3" => std::fs::write(path.join("notes.txt"), "任务列表\nTODO: 校对\n")?,
        "C4" => {
            std::fs::write(path.join("a.txt"), "A17\nB42\n")?;
            std::fs::write(path.join("b.txt"), "B42\nC09\n")?;
        }
        "C5" => {
            std::fs::create_dir_all(path.join("src"))?;
            std::fs::write(
                path.join("src/app.js"),
                "export function multiply(a, b) { return a + b; }\n",
            )?;
        }
        _ => {}
    }
    Ok(())
}

fn assistant_text(export: &Value) -> String {
    export["messages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|m| m["role"] == "assistant")
        .filter_map(|m| m["content"].as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

fn has_final_answer(export: &Value) -> bool {
    export["messages"]
        .as_array()
        .and_then(|messages| messages.iter().rev().find(|m| m["role"] == "assistant"))
        .is_some_and(|message| {
            message["content"]
                .as_str()
                .is_some_and(|text| !text.trim().is_empty())
                && message["toolCalls"].is_null()
        })
}

fn file_verdict(path: &Path, id: &str) -> bool {
    match id {
        "C1" => std::fs::read_to_string(path.join("summary.md"))
            .is_ok_and(|s| s.contains("add") && (s.contains("加") || s.contains("和"))),
        "C2" => std::fs::read_to_string(path.join("config.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<Value>(&s).ok())
            .is_some_and(|v| v["mode"] == "ready" && v["retries"] == 2),
        "C3" => std::fs::read_to_string(path.join("notes.txt"))
            .is_ok_and(|s| s.contains("DONE: 校对") && !s.contains("TODO: 校对")),
        "C4" => std::fs::read_to_string(path.join("report.txt"))
            .is_ok_and(|s| s.contains("B42") && !s.contains("A17") && !s.contains("C09")),
        "C5" => std::fs::read_to_string(path.join("src/app.js"))
            .is_ok_and(|s| s.contains("a * b") || s.contains("a*b")),
        _ => false,
    }
}

fn text_verdict(id: &str, text: &str) -> bool {
    match id {
        "D1" => text.contains("中文") && text.contains("简短") && text.contains("蓝色风筝"),
        "D2" => {
            text.contains("2028")
                && text.contains("4")
                && text.contains("18")
                && text.contains("李青")
        }
        "D3" => text.contains("360") && text.contains("海风公园") && text.contains('6'),
        "D4" => ["买菜", "运动", "阅读"]
            .iter()
            .all(|part| text.contains(part)),
        "D5" => text.contains("60"),
        _ => false,
    }
}

#[tokio::main(flavor = "multi_thread", worker_threads = 4)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = std::env::var("DANDELION_TEST_MLX_MODEL")?;
    let runner = std::env::var("DANDELION_MLX_SERVER")?;
    if !Path::new(&runner).is_file() {
        return Err("MLX runner missing".into());
    }
    let stamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    let root = std::env::temp_dir().join(format!("dandelion-agent-eval-{stamp}"));
    let data = root.join("data");
    std::fs::create_dir_all(&data)?;

    let mut settings = Settings::defaults();
    settings.default_backend = "sidecar".into();
    settings.base_url = Some("http://127.0.0.1:9/v1".into());
    settings.default_model = Some("pending".into());
    settings.workspace = Some(root.clone());
    settings.permission_mode = PermissionMode::Auto;
    settings.allow_shell = false;
    settings.max_local_concurrency = 2;
    settings.thinking_intensity = ThinkingIntensity::Low;
    settings.max_tokens = 512;
    settings.max_iterations = 6;
    settings.corpus_min_disk_free_mb = 1;
    settings.save(&data)?;
    let rt = Runtime::new(Arc::new(CliHost::with_data_dir(data)))?;
    let row = rt.local_model_import_mlx(Path::new(&model))?;
    rt.local_model_activate(&row.id)?;
    rt.memory_set(
        MemoryKind::User,
        "用户偏好：用中文和简短回答；日常建议清单最多三项。",
    )?;
    rt.memory_set(MemoryKind::Memory, "测试口令：蓝色风筝。")?;
    let knowledge_path = root.join("knowledge.txt");
    std::fs::write(&knowledge_path, "项目晨星的发布日期是2028年4月18日，负责人是李青。周末活动预算360元，地点海风公园，参加人数6人。")?;
    let corpus = rt.corpus_import(&knowledge_path)?;

    // The separate customer knowledge space is tested independently, because
    // it is not part of the Agent's automatic corpus retrieval path.
    let space = rt.knowledge_create_local_space("local-user", "综合测试资料", "local_only")?;
    let (doc, version) = rt.knowledge_import_document(
        "local",
        "local",
        &space.id,
        "local-user",
        "manual",
        "eval-knowledge",
        "项目晨星的发布日期是2028年4月18日，负责人是李青。",
        256,
    )?;
    rt.knowledge_publish_version(
        "local",
        "local",
        &space.id,
        "local-user",
        &doc.id,
        &version.id,
    )?;
    let knowledge_answer = rt.knowledge_answer(
        "local",
        "local",
        &space.id,
        "local-user",
        "项目晨星负责人是谁？",
        5,
    )?;

    println!("EVAL_ROOT={}", root.display());
    println!("MODEL={}", row.name);
    rt.sidecar_start().await?;
    println!("SIDECAR_READY");

    let smoke = std::env::var_os("DANDELION_EVAL_SMOKE").is_some();
    let selected = tasks()
        .into_iter()
        .filter(|t| !smoke || t.id == "C1" || t.id == "D3")
        .collect::<Vec<_>>();
    let done = Arc::new(AtomicBool::new(false));
    let peak = Arc::new(AtomicUsize::new(0));
    let monitor_rt = rt.clone();
    let monitor_done = done.clone();
    let monitor_peak = peak.clone();
    let monitor = tokio::spawn(async move {
        while !monitor_done.load(Ordering::Relaxed) {
            let waiting = monitor_rt.sidecar_status().queue.waiting;
            monitor_peak.fetch_max(waiting, Ordering::Relaxed);
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });

    let started = Instant::now();
    let mut jobs = Vec::new();
    for task in selected {
        let workspace = root.join(task.id);
        seed_workspace(&workspace, task.id)?;
        let sid = rt.create_session(Some(task.id))?;
        if task.id == "D3" {
            rt.session_replace_bindings(&sid, "knowledge", &[space.id.clone()])?;
        }
        let mut request = PromptRequest::new(sid.clone(), task.prompt);
        request.workspace = Some(workspace.clone());
        request.task_mode = if task.code {
            TaskMode::Auto
        } else {
            TaskMode::Ask
        };
        request.max_tokens = Some(if task.code { 768 } else { 384 });
        if task.corpus {
            request.corpus_ids = vec![corpus.id.clone()];
        }
        let worker = rt.clone();
        jobs.push(tokio::spawn(async move {
            let t0 = Instant::now();
            let result =
                tokio::time::timeout(Duration::from_secs(600), worker.prompt(request)).await;
            let error = match result {
                Ok(Ok(_)) => None,
                Ok(Err(err)) => Some(err.to_string()),
                Err(_) => {
                    let _ = worker.cancel(sid.clone()).await;
                    Some("timeout after 600 seconds including queue wait".into())
                }
            };
            let export = worker
                .session_export(&sid)
                .unwrap_or_else(|e| json!({"error": e.to_string()}));
            let text = assistant_text(&export);
            let output_passed = if task.code {
                file_verdict(&workspace, task.id)
            } else {
                text_verdict(task.id, &text)
            };
            let final_answer_present = has_final_answer(&export);
            let clean_completion = error.is_none() && final_answer_present;
            let tool_calls = export["messages"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|m| {
                    m["toolCalls"]
                        .as_str()
                        .is_some_and(|s| !s.is_empty() && s != "[]")
                })
                .count();
            json!({
                "id": task.id, "code": task.code,
                "outputPassed": output_passed, "cleanCompletion": clean_completion,
                "finalAnswerPresent": final_answer_present,
                "error": error,
                "elapsedSec": t0.elapsed().as_secs_f64(), "toolCallMessages": tool_calls,
                "assistantText": text, "export": export,
            })
        }));
    }
    let mut results = Vec::new();
    for job in jobs {
        results.push(job.await?);
    }
    done.store(true, Ordering::Relaxed);
    monitor.await?;
    rt.sidecar_stop();
    let summary = json!({
        "root": root, "model": row.name, "elapsedSec": started.elapsed().as_secs_f64(),
        "submitted": results.len(), "peakWaiting": peak.load(Ordering::Relaxed),
        "outputPassed": results.iter().filter(|r| r["outputPassed"] == true).count(),
        "cleanCompleted": results.iter().filter(|r| r["cleanCompletion"] == true).count(),
        "knowledgeAnswer": serde_json::to_value(knowledge_answer)?,
        "tasks": results,
    });
    let report =
        PathBuf::from(summary["root"].as_str().ok_or("missing root")?).join("results.json");
    std::fs::write(&report, serde_json::to_vec_pretty(&summary)?)?;
    println!("EVAL_REPORT={}", report.display());
    println!(
        "OUTPUT_PASSED={}/{} CLEAN_COMPLETED={}/{} PEAK_WAITING={} ELAPSED={:.1}s",
        summary["outputPassed"],
        summary["submitted"],
        summary["cleanCompleted"],
        summary["submitted"],
        summary["peakWaiting"],
        summary["elapsedSec"].as_f64().unwrap_or(0.0)
    );
    Ok(())
}
