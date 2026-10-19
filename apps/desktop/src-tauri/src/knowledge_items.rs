//! Host-owned pickers and export. Persistence and model work stay in core.
use super::*;
use dandelion_core::store::sqlite::{KnowledgeItem, KnowledgeItemInput};
const SUBJECT: &str = "local-user";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CollectResult {
    items: Vec<KnowledgeItem>,
    failures: Vec<KnowledgeFileImportFailure>,
    skipped: Vec<String>,
}

#[tauri::command]
pub async fn knowledge_item_list(
    state: State<'_, AppState>,
    query: Option<String>,
) -> Result<Vec<KnowledgeItem>, String> {
    let rt = state.runtime.read().await.clone();
    tokio::task::spawn_blocking(move || {
        rt.knowledge_item_list(SUBJECT, query.as_deref().unwrap_or(""))
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(map_err)
}
#[tauri::command]
pub async fn knowledge_item_get(
    state: State<'_, AppState>,
    id: String,
) -> Result<KnowledgeItem, String> {
    state
        .runtime
        .read()
        .await
        .knowledge_item_get(SUBJECT, &id)
        .map_err(map_err)
}
#[tauri::command]
pub async fn knowledge_item_save(
    state: State<'_, AppState>,
    input: KnowledgeItemInput,
) -> Result<KnowledgeItem, String> {
    let rt = state.runtime.read().await.clone();
    tokio::task::spawn_blocking(move || rt.knowledge_item_save(SUBJECT, &input))
        .await
        .map_err(|e| e.to_string())?
        .map_err(map_err)
}
#[tauri::command]
pub async fn knowledge_item_retry(
    state: State<'_, AppState>,
    id: String,
) -> Result<KnowledgeItem, String> {
    let rt = state.runtime.read().await.clone();
    tokio::task::spawn_blocking(move || rt.knowledge_item_retry(SUBJECT, &id))
        .await
        .map_err(|e| e.to_string())?
        .map_err(map_err)
}
#[tauri::command]
pub async fn knowledge_item_organize(
    state: State<'_, AppState>,
    id: String,
    version_id: String,
    topics: Option<Vec<String>>,
    favorite: Option<bool>,
) -> Result<(), String> {
    state
        .runtime
        .read()
        .await
        .knowledge_item_organize(SUBJECT, &id, &version_id, topics, favorite)
        .map_err(map_err)
}
#[tauri::command]
pub async fn knowledge_item_ai_policy(
    state: State<'_, AppState>,
    enabled: Option<bool>,
) -> Result<bool, String> {
    state
        .runtime
        .read()
        .await
        .knowledge_item_ai_policy(SUBJECT, enabled)
        .map_err(map_err)
}
#[tauri::command]
pub async fn knowledge_item_classify(
    state: State<'_, AppState>,
    id: String,
) -> Result<KnowledgeItem, String> {
    let rt = state.runtime.read().await.clone();
    rt.knowledge_item_classify(SUBJECT, &id)
        .await
        .map_err(map_err)
}
#[tauri::command]
pub async fn knowledge_item_delete(state: State<'_, AppState>, id: String) -> Result<(), String> {
    state
        .runtime
        .read()
        .await
        .knowledge_item_delete(SUBJECT, &id)
        .map_err(map_err)
}
#[tauri::command]
pub async fn knowledge_item_save_draft(
    state: State<'_, AppState>,
    draft_id: String,
) -> Result<KnowledgeItem, String> {
    state
        .runtime
        .read()
        .await
        .knowledge_item_save_draft(&draft_id)
        .map_err(map_err)
}

fn collect_folder(root: &Path, paths: &mut Vec<PathBuf>, skipped: &mut Vec<String>) {
    let entries = match std::fs::read_dir(root) {
        Ok(e) => e,
        Err(e) => {
            skipped.push(format!("{}：{e}", root.display()));
            return;
        }
    };
    for entry in entries {
        let entry = match entry {
            Ok(e) => e,
            Err(e) => {
                skipped.push(e.to_string());
                continue;
            }
        };
        let path = entry.path();
        let name = entry.file_name().to_string_lossy().into_owned();
        let kind = match entry.file_type() {
            Ok(t) => t,
            Err(e) => {
                skipped.push(format!("{}：{e}", path.display()));
                continue;
            }
        };
        if kind.is_symlink()
            || name.starts_with('.')
            || matches!(
                name.as_str(),
                "node_modules" | "target" | "dist" | "build" | "vendor"
            )
            || matches!(
                path.extension().and_then(|x| x.to_str()),
                Some("pem" | "key" | "p12" | "pfx")
            )
        {
            skipped.push(format!(
                "{}：隐藏文件、链接、构建目录或凭据文件",
                path.display()
            ));
            continue;
        }
        if kind.is_dir() {
            collect_folder(&path, paths, skipped);
        } else if kind.is_file() {
            paths.push(path);
        }
    }
}
#[tauri::command]
pub async fn knowledge_items_pick(
    state: State<'_, AppState>,
    folder: bool,
) -> Result<Option<CollectResult>, String> {
    let rt = state.runtime.read().await.clone();
    tokio::task::spawn_blocking(move||{
        let picker=rfd::FileDialog::new().set_title(if folder{"选择要收集的文件夹"}else{"添加知识文件（原件保存在本机）"});
        let selected=if folder{picker.pick_folder().map(|p|vec![p])}else{picker.pick_files()};
        let Some(selected)=selected else{return Ok(None);};
        let mut paths=Vec::new();let mut skipped=Vec::new();
        for p in selected {if p.is_dir(){collect_folder(&p,&mut paths,&mut skipped);}else{paths.push(p);}}
        paths.sort();paths.dedup();
        if folder {
            let preview=paths.iter().map(|p|p.file_name().unwrap_or_default().to_string_lossy()).take(12).collect::<Vec<_>>().join("\n");
            let confirmed=rfd::MessageDialog::new().set_title("确认收集范围").set_description(format!("将保存 {} 个文件的本地副本，跳过 {} 个目录或文件。\n\n{}{}\n\n这是一次性收集，之后可在列表查看处理结果。",paths.len(),skipped.len(),preview,if paths.len()>12{"\n…其余文件也将保存"}else{""})).set_buttons(rfd::MessageButtons::OkCancel).show();
            if confirmed!=rfd::MessageDialogResult::Ok{return Ok(None);}
        }
        let mut items=Vec::new();let mut failures=Vec::new();
        for path in paths {match rt.knowledge_item_collect(SUBJECT,&path){Ok(item)=>items.push(item),Err(e)=>failures.push(KnowledgeFileImportFailure{source_id:path.to_string_lossy().into_owned(),message:e.to_string()})}}
        Ok::<_,String>(Some(CollectResult{items,failures,skipped}))
    }).await.map_err(|e|e.to_string())?
}

#[tauri::command]
pub async fn knowledge_item_export(
    state: State<'_, AppState>,
    id: String,
    original: bool,
) -> Result<Option<String>, String> {
    let rt = state.runtime.read().await.clone();
    tokio::task::spawn_blocking(move || {
        let item = rt.knowledge_item_get(SUBJECT, &id).map_err(map_err)?;
        let source = if original {
            Some(rt.knowledge_item_asset(SUBJECT, &id).map_err(map_err)?)
        } else {
            None
        };
        let name = if original {
            Path::new(&item.source_id)
                .file_name()
                .and_then(|s| s.to_str())
                .unwrap_or("原件")
                .to_owned()
        } else {
            format!("{}.md", item.title.replace(['/', '\\', ':'], "_"))
        };
        let Some(dest) = rfd::FileDialog::new().set_file_name(name).save_file() else {
            return Ok(None);
        };
        if let Some(source) = source {
            std::fs::copy(source, &dest).map_err(|e| e.to_string())?;
        } else {
            std::fs::write(
                &dest,
                format!(
                    "# {}\n\n{}\n\n## 我的笔记\n{}\n\n来源：{}\n主题：{}\n",
                    item.title,
                    item.body,
                    item.notes,
                    item.source_id,
                    item.topics.join("、")
                ),
            )
            .map_err(|e| e.to_string())?;
        }
        Ok::<_, String>(Some(dest.to_string_lossy().into_owned()))
    })
    .await
    .map_err(|e| e.to_string())?
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn folder_collection_keeps_mixed_knowledge_and_reports_exclusions() {
        let t = tempfile::tempdir().unwrap();
        std::fs::write(t.path().join("README.md"), "").unwrap();
        std::fs::write(t.path().join("snippet.go"), "package main").unwrap();
        std::fs::write(t.path().join("lesson.mp4"), [0u8, 255]).unwrap();
        std::fs::write(t.path().join(".env"), "private").unwrap();
        std::fs::create_dir(t.path().join("node_modules")).unwrap();
        std::fs::write(t.path().join("node_modules/dep.js"), "ignored").unwrap();
        let mut files = Vec::new();
        let mut skipped = Vec::new();
        collect_folder(t.path(), &mut files, &mut skipped);
        assert_eq!(files.len(), 3);
        assert_eq!(skipped.len(), 2);
        assert!(files.iter().any(|p| p.extension().unwrap() == "mp4"));
    }
}
