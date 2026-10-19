//! Personal content items reuse the permissioned document/version store.
use super::*;
use serde::{Deserialize, Serialize};
use std::io::{Read, Write};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeItemInput {
    pub title: String,
    pub kind: String,
    pub body: String,
    #[serde(default)]
    pub notes: String,
    pub document_id: Option<String>,
    pub expected_version_id: Option<String>,
    pub source_id: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct KnowledgeItem {
    pub id: String,
    pub space_id: String,
    pub version_id: String,
    pub title: String,
    pub kind: String,
    pub source_id: String,
    pub body: String,
    pub notes: String,
    pub has_asset: bool,
    pub parse_status: String,
    pub parse_error: Option<String>,
    pub topics: Vec<String>,
    pub suggested_topics: Vec<String>,
    pub favorite: bool,
    pub classification_status: String,
    pub classification_error: Option<String>,
    pub updated_at: String,
    pub enabled: bool,
    pub model_allowed: bool,
    pub manual_topics: bool,
}

impl Store {
    pub fn knowledge_latest_request(&self, session: &crate::SessionId) -> Result<String> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        Ok(conn.query_row("SELECT content FROM messages WHERE session_id=?1 AND role='user' ORDER BY id DESC LIMIT 1",[session.as_str()],|r|r.get(0)).optional()?.unwrap_or_default())
    }

    pub fn knowledge_personal_space(&self, subject: &str) -> Result<KnowledgeSpace> {
        if subject.trim().is_empty() {
            return Err(Error::Invalid("需要本地身份".into()));
        }
        self.knowledge_local_spaces()?;
        let id = {
            let mut conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
            let tx = conn.transaction()?;
            let found: Option<String> = tx
                .query_row(
                    "SELECT p.space_id FROM knowledge_personal_spaces p JOIN knowledge_spaces s ON s.id=p.space_id JOIN knowledge_space_members m ON m.space_id=s.id AND m.subject_id=p.subject_id WHERE p.subject_id=?1 AND s.archived_at IS NULL AND m.role='\"owner\"'",
                    [subject],
                    |r| r.get(0),
                )
                .optional()?;
            let reusable: Option<String> = if found.is_none() {
                tx.query_row("SELECT s.id FROM knowledge_spaces s JOIN knowledge_space_members m ON m.space_id=s.id WHERE s.tenant_id='local' AND s.customer_id='local' AND s.name='我的知识' AND s.kind='docs' AND s.archived_at IS NULL AND m.subject_id=?1 AND m.role='\"owner\"'",[subject],|r|r.get(0)).optional()?
            } else {
                None
            };
            let id = if let Some(id) = found.or(reusable) {
                id
            } else {
                let id = format!("space_{}", uuid::Uuid::new_v4().simple());
                let name_taken: bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM knowledge_spaces WHERE tenant_id='local' AND customer_id='local' AND name='我的知识')",[],|r|r.get(0))?;
                let name = if name_taken {
                    format!("我的知识-{}", &id[id.len() - 8..])
                } else {
                    "我的知识".into()
                };
                let now = Utc::now().to_rfc3339();
                tx.execute("INSERT INTO knowledge_spaces (id,tenant_id,customer_id,name,external_policy,created_at,kind) VALUES (?1,'local','local',?3,'local_only',?2,'docs')", params![id, now,name])?;
                tx.execute("INSERT INTO knowledge_space_members (space_id,subject_id,role,created_at) VALUES (?1,?2,'\"owner\"',?3)", params![id,subject,now])?;
                id
            };
            tx.execute("INSERT INTO knowledge_personal_spaces (subject_id,space_id) VALUES (?1,?2) ON CONFLICT(subject_id) DO UPDATE SET space_id=excluded.space_id", params![subject,id])?;
            tx.commit()?;
            id
        };
        self.knowledge_authorize(&id, subject, KnowledgeRole::Owner)?;
        self.knowledge_space_by_id(&id)?
            .ok_or_else(|| Error::Invalid("个人知识库不存在".into()))
    }

    /// Summaries use SQLite projection: opening a library never loads every
    /// attachment's entire extracted body into the WebView. Search still
    /// checks full stored content, not just the visible excerpt.
    pub fn knowledge_item_summaries(
        &self,
        subject: &str,
        query: &str,
    ) -> Result<Vec<KnowledgeItem>> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let mut statement=conn.prepare(r#"
            SELECT d.id,d.space_id,v.id,COALESCE(i.title,d.source_id),COALESCE(i.kind,'article'),d.source_id,
              substr(COALESCE(i.body,v.content),1,180),substr(COALESCE(i.notes,''),1,180),i.asset_path IS NOT NULL,
              COALESCE(i.parse_status,'ready'),i.parse_error,COALESCE(i.topics,'[]'),COALESCE(i.suggested_topics,'[]'),
              COALESCE(i.favorite,0),COALESCE(i.classification_status,'pending'),i.classification_error,
              COALESCE(i.updated_at,v.created_at),v.published_at IS NOT NULL AND v.revoked_at IS NULL AND v.superseded_at IS NULL,
              s.external_policy='allow_summary',COALESCE(i.manual_topics,0)
            FROM knowledge_documents d JOIN knowledge_spaces s ON s.id=d.space_id
            JOIN knowledge_space_members m ON m.space_id=s.id AND m.subject_id=?1
            JOIN knowledge_document_versions v ON v.document_id=d.id AND v.version_no=(SELECT MAX(v2.version_no) FROM knowledge_document_versions v2 WHERE v2.document_id=d.id)
            LEFT JOIN knowledge_item_details i ON i.document_id=d.id AND i.version_id=v.id
            WHERE d.deleted_at IS NULL AND s.tenant_id='local' AND s.customer_id='local' AND s.kind<>'wiki' AND s.archived_at IS NULL
              AND m.role IN ('"owner"','"editor"','"reader"')
              AND ((v.published_at IS NOT NULL AND v.revoked_at IS NULL AND v.superseded_at IS NULL) OR m.role IN ('"owner"','"editor"'))
              AND (?2='' OR instr(lower(v.content),lower(?2))>0 OR instr(lower(d.source_id),lower(?2))>0 OR instr(lower(COALESCE(i.title,'')),lower(?2))>0)
            ORDER BY COALESCE(i.updated_at,v.created_at) DESC,d.id
        "#)?;
        let rows = statement.query_map(params![subject, query.trim()], |r| {
            Ok(KnowledgeItem {
                id: r.get(0)?,
                space_id: r.get(1)?,
                version_id: r.get(2)?,
                title: r.get(3)?,
                kind: r.get(4)?,
                source_id: r.get(5)?,
                body: r.get(6)?,
                notes: r.get(7)?,
                has_asset: r.get(8)?,
                parse_status: r.get(9)?,
                parse_error: r.get(10)?,
                topics: serde_json::from_str(&r.get::<_, String>(11)?).unwrap_or_default(),
                suggested_topics: serde_json::from_str(&r.get::<_, String>(12)?)
                    .unwrap_or_default(),
                favorite: r.get(13)?,
                classification_status: r.get(14)?,
                classification_error: r.get(15)?,
                updated_at: r.get(16)?,
                enabled: r.get(17)?,
                model_allowed: r.get(18)?,
                manual_topics: r.get(19)?,
            })
        })?;
        rows.collect::<std::result::Result<Vec<_>, _>>()
            .map_err(Into::into)
    }

    pub fn knowledge_items(&self, subject: &str) -> Result<Vec<KnowledgeItem>> {
        self.knowledge_item_summaries(subject, "")?
            .into_iter()
            .map(|i| self.knowledge_item(subject, &i.id))
            .collect()
    }

    fn item_by_source(&self, subject: &str, source: &str) -> Result<Option<KnowledgeItem>> {
        let id: Option<String> = {
            let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
            conn.query_row("SELECT id FROM knowledge_documents WHERE tenant_id='local' AND customer_id='local' AND source_id=?1 AND deleted_at IS NULL ORDER BY (source_kind='personal_item') DESC,created_at DESC LIMIT 1",[source],|r|r.get(0)).optional()?
        };
        id.map(|id| self.knowledge_item(subject, &id)).transpose()
    }

    fn item_home(&self, document: &str) -> Result<String> {
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        conn.query_row(
            "SELECT space_id FROM knowledge_documents WHERE id=?1 AND deleted_at IS NULL",
            [document],
            |r| r.get(0),
        )
        .optional()?
        .ok_or_else(|| Error::Invalid("知识条目不存在或已删除".into()))
    }

    pub fn knowledge_item(&self, subject: &str, document: &str) -> Result<KnowledgeItem> {
        let home = self.item_home(document)?;
        let preview = self.knowledge_document_preview(&home, subject, document)?;
        let model_allowed = self
            .knowledge_space_by_id(&home)?
            .is_some_and(|s| s.external_policy == "allow_summary");
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let mut item = KnowledgeItem {
            id: document.into(),
            space_id: home,
            version_id: preview.version_id.clone(),
            title: preview
                .source_id
                .rsplit(['/', '\\'])
                .next()
                .unwrap_or(&preview.source_id)
                .into(),
            kind: "article".into(),
            source_id: preview.source_id,
            body: preview.content,
            notes: String::new(),
            has_asset: false,
            parse_status: "ready".into(),
            parse_error: None,
            topics: vec![],
            suggested_topics: vec![],
            favorite: false,
            classification_status: "pending".into(),
            classification_error: None,
            updated_at: String::new(),
            enabled: preview.enabled,
            model_allowed,
            manual_topics: false,
        };
        let details = conn.query_row("SELECT version_id,title,kind,body,notes,asset_path,parse_status,parse_error,topics,suggested_topics,favorite,classification_status,classification_error,updated_at,manual_topics FROM knowledge_item_details WHERE document_id=?1",[document],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?,r.get::<_,String>(3)?,r.get::<_,String>(4)?,r.get::<_,Option<String>>(5)?,r.get::<_,String>(6)?,r.get::<_,Option<String>>(7)?,r.get::<_,String>(8)?,r.get::<_,String>(9)?,r.get::<_,bool>(10)?,r.get::<_,String>(11)?,r.get::<_,Option<String>>(12)?,r.get::<_,String>(13)?,r.get::<_,bool>(14)?))).optional()?;
        if let Some((
            v,
            title,
            kind,
            body,
            notes,
            asset,
            status,
            error,
            topics,
            suggestions,
            favorite,
            class,
            class_error,
            updated,
            manual,
        )) = details
        {
            if v == preview.version_id {
                item.title = title;
                item.kind = kind;
                item.body = body;
                item.notes = notes;
                item.has_asset = asset.is_some();
                item.parse_status = status;
                item.parse_error = error;
                item.topics = serde_json::from_str(&topics).unwrap_or_default();
                item.suggested_topics = serde_json::from_str(&suggestions).unwrap_or_default();
                item.favorite = favorite;
                item.classification_status = class;
                item.classification_error = class_error;
                item.updated_at = updated;
                item.manual_topics = manual;
            }
        }
        if item.updated_at.is_empty() {
            item.updated_at = conn.query_row(
                "SELECT created_at FROM knowledge_document_versions WHERE id=?1",
                [&item.version_id],
                |r| r.get(0),
            )?;
        }
        Ok(item)
    }

    pub fn knowledge_save_item(
        &self,
        subject: &str,
        input: &KnowledgeItemInput,
    ) -> Result<KnowledgeItem> {
        self.save_item_with_asset(subject, input, None)
    }

    fn save_item_with_asset(
        &self,
        subject: &str,
        input: &KnowledgeItemInput,
        asset: Option<(&str, &str, &str, Option<&str>)>,
    ) -> Result<KnowledgeItem> {
        let _write = self
            .item_write
            .lock()
            .map_err(|_| Error::Store("lock".into()))?;
        let title = if input.title.trim().is_empty() {
            input
                .body
                .lines()
                .chain(input.notes.lines())
                .find(|line| !line.trim().is_empty())
                .map(|line| {
                    line.trim()
                        .trim_start_matches('#')
                        .trim()
                        .chars()
                        .take(60)
                        .collect::<String>()
                })
                .filter(|line| !line.is_empty())
                .unwrap_or_else(|| "未命名笔记".into())
        } else {
            input.title.trim().to_owned()
        };
        if !matches!(
            input.kind.as_str(),
            "article" | "note" | "code" | "link" | "video" | "image" | "file"
        ) {
            return Err(Error::Invalid("不支持的知识类型".into()));
        }
        let (space, source) = if let Some(id) = &input.document_id {
            let old = self.knowledge_item(subject, id)?;
            self.knowledge_authorize(&old.space_id, subject, KnowledgeRole::Editor)?;
            if input.expected_version_id.as_deref() != Some(old.version_id.as_str()) {
                return Err(Error::Invalid(
                    "内容已更新，请刷新后再编辑，避免覆盖新版本".into(),
                ));
            }
            (
                self.knowledge_space_by_id(&old.space_id)?
                    .ok_or_else(|| Error::Invalid("知识库不存在".into()))?,
                old.source_id,
            )
        } else {
            (
                self.knowledge_personal_space(subject)?,
                input
                    .source_id
                    .clone()
                    .unwrap_or_else(|| format!("note:{}", uuid::Uuid::new_v4().simple())),
            )
        };
        // Metadata-only attachments explicitly state the indexing boundary.
        let body = input.body.trim();
        let mut content = format!(
            "# {title}\n\n{}{}{}",
            if body.is_empty() {
                "（原件暂无可检索正文，仅索引标题和笔记。）"
            } else {
                body
            },
            if input.notes.trim().is_empty() {
                ""
            } else {
                "\n\n## 我的笔记\n"
            },
            input.notes.trim()
        );
        // Binary/empty attachments can have identical extracted text while
        // their originals differ. Include snapshot identity in the immutable
        // document version so a changed video cannot reuse an old version.
        let snapshot = if let Some(asset) = asset {
            Some(asset.0.to_owned())
        } else if let Some(id) = &input.document_id {
            let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
            conn.query_row(
                "SELECT asset_path FROM knowledge_item_details WHERE document_id=?1",
                [id],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten()
        } else {
            None
        };
        if let Some(snapshot) = snapshot {
            let hash = snapshot
                .rsplit('/')
                .next()
                .unwrap_or(&snapshot)
                .split('.')
                .next()
                .unwrap_or("");
            content.push_str(&format!("\n\n附件快照标识：{hash}"));
        }
        let source_kind = if let Some(id) = &input.document_id {
            let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
            conn.query_row(
                "SELECT source_kind FROM knowledge_documents WHERE id=?1",
                [id],
                |r| r.get::<_, String>(0),
            )?
        } else {
            "personal_item".into()
        };
        let (doc, version) = self.knowledge_import_document(
            &space.tenant_id,
            &space.customer_id,
            &space.id,
            subject,
            &source_kind,
            &source,
            &content,
            1200,
        )?;
        {
            let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
            conn.execute("INSERT INTO knowledge_item_details (document_id,version_id,title,kind,body,notes,asset_path,asset_name,parse_status,parse_error,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11) ON CONFLICT(document_id) DO UPDATE SET version_id=excluded.version_id,title=excluded.title,kind=excluded.kind,body=excluded.body,notes=excluded.notes,asset_path=COALESCE(excluded.asset_path,knowledge_item_details.asset_path),asset_name=COALESCE(excluded.asset_name,knowledge_item_details.asset_name),parse_status=CASE WHEN excluded.asset_path IS NULL THEN knowledge_item_details.parse_status ELSE excluded.parse_status END,parse_error=CASE WHEN excluded.asset_path IS NULL THEN knowledge_item_details.parse_error ELSE excluded.parse_error END,classification_status=CASE WHEN knowledge_item_details.manual_topics=1 THEN 'complete' WHEN knowledge_item_details.version_id=excluded.version_id THEN knowledge_item_details.classification_status ELSE 'pending' END,classification_error=NULL,updated_at=excluded.updated_at",params![doc.id,version.id,title,input.kind,body,input.notes,asset.map(|a|a.0),asset.map(|a|a.1),asset.map(|a|a.2).unwrap_or("ready"),asset.and_then(|a|a.3),Utc::now().to_rfc3339()])?;
        }
        {
            let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
            conn.execute("INSERT OR IGNORE INTO knowledge_item_asset_versions(version_id,asset_path,asset_name) SELECT version_id,asset_path,COALESCE(asset_name,'原件') FROM knowledge_item_details WHERE document_id=?1 AND asset_path IS NOT NULL",[&doc.id])?;
        }
        self.knowledge_publish_version(
            &space.tenant_id,
            &space.customer_id,
            &space.id,
            subject,
            &doc.id,
            &version.id,
        )?;
        self.knowledge_item(subject, &doc.id)
    }

    /// Preserve first, then extract. Assets are immutable content-addressed
    /// snapshots. A parser error cannot delete the snapshot or the item.
    pub fn knowledge_collect_file(
        &self,
        subject: &str,
        path: &Path,
        text_limit: usize,
    ) -> Result<KnowledgeItem> {
        let _assets = self
            .item_assets
            .lock()
            .map_err(|_| Error::Store("lock".into()))?;
        self.collect_file_at(subject, path, None, text_limit)
    }

    pub fn knowledge_retry_item(
        &self,
        subject: &str,
        document: &str,
        text_limit: usize,
    ) -> Result<KnowledgeItem> {
        let _assets = self
            .item_assets
            .lock()
            .map_err(|_| Error::Store("lock".into()))?;
        let item = self.knowledge_item(subject, document)?;
        self.knowledge_authorize(&item.space_id, subject, KnowledgeRole::Editor)?;
        let asset = self.knowledge_item_asset(subject, document)?;
        self.collect_file_at(subject, &asset, Some(&item.source_id), text_limit)
    }

    fn collect_file_at(
        &self,
        subject: &str,
        path: &Path,
        logical_source: Option<&str>,
        text_limit: usize,
    ) -> Result<KnowledgeItem> {
        self.knowledge_personal_space(subject)?;
        let name = Path::new(logical_source.unwrap_or_else(|| path.to_str().unwrap_or("")))
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| Error::Invalid("文件名不可读取".into()))?;
        let root = self.data_dir.join("knowledge/assets");
        std::fs::create_dir_all(&root)?;
        let temp = root.join(format!("{}.part", uuid::Uuid::new_v4().simple()));
        let copied = (|| -> Result<(String, u64)> {
            let mut source = std::fs::File::open(path)?;
            let mut dest = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&temp)?;
            let mut hash = Sha256::new();
            let mut buf = [0u8; 65536];
            let mut size = 0;
            loop {
                let n = source.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                hash.update(&buf[..n]);
                dest.write_all(&buf[..n])?;
                size += n as u64;
            }
            dest.sync_all()?;
            Ok((format!("{:x}", hash.finalize()), size))
        })();
        let (hash, size) = match copied {
            Ok(v) => v,
            Err(e) => {
                let _ = std::fs::remove_file(&temp);
                return Err(e);
            }
        };
        let ext = path
            .extension()
            .and_then(|e| e.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        let safe_ext: String = ext
            .chars()
            .filter(|c| c.is_ascii_alphanumeric())
            .take(16)
            .collect();
        let filename = format!("{hash}.{safe_ext}");
        let snapshot = root.join(&filename);
        if snapshot.exists() {
            std::fs::remove_file(&temp)?;
        } else {
            std::fs::rename(&temp, &snapshot)?;
        }
        let relative = format!("knowledge/assets/{filename}");
        let kind = match ext.as_str() {
            "mp4" | "mov" | "mkv" | "webm" => "video",
            "png" | "jpg" | "jpeg" | "gif" | "webp" => "image",
            "go" | "rs" | "js" | "ts" | "tsx" | "jsx" | "vue" | "svelte" | "py" | "java" | "kt" | "swift" | "cs" | "rb" | "php" | "lua" | "sql" | "css" | "sh" | "bash" | "ps1" | "xml" | "ini" | "conf" | "log" | "csv" | "json"
            | "yaml" | "yml" | "toml" | "c" | "cpp" | "h" => "code",
            _ => "file",
        };
        let text_format = matches!(
            ext.as_str(),
            "md" | "markdown" | "mdx" | "txt" | "rst" | "adoc"
        ) || kind == "code";
        let source = logical_source
            .map(str::to_owned)
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        let existing = self.item_by_source(subject, &source)?;
        if let Some(old) = &existing {
            if matches!(old.parse_status.as_str(), "ready" | "empty" | "unsupported")
                && self
                    .knowledge_item_asset(subject, &old.id)
                    .is_ok_and(|p| p == snapshot)
            {
                return Ok(old.clone());
            }
        }
        let mut input = KnowledgeItemInput {
            title: existing
                .as_ref()
                .map(|i| i.title.clone())
                .unwrap_or_else(|| name.into()),
            kind: if text_format && kind != "code" {
                "article"
            } else {
                kind
            }
            .into(),
            body: String::new(),
            notes: existing
                .as_ref()
                .map(|i| i.notes.clone())
                .unwrap_or_default(),
            document_id: existing.as_ref().map(|i| i.id.clone()),
            expected_version_id: existing.as_ref().map(|i| i.version_id.clone()),
            source_id: Some(source),
        };
        // Durable item first. Even a process kill inside a parser leaves an
        // accessible original and a retryable pending row.
        let saved = self.save_item_with_asset(
            subject,
            &input,
            Some((&relative, name, "pending", Some("原件已保存，等待提取正文"))),
        )?;
        input.document_id = Some(saved.id);
        input.expected_version_id = Some(saved.version_id);
        let parsed: std::result::Result<String, String> = if size == 0 {
            Ok(String::new())
        } else if text_format {
            if size > text_limit as u64 {
                Err(format!(
                    "文件已保存；正文超过当前解析上限 {text_limit} 字节"
                ))
            } else {
                std::fs::read_to_string(&snapshot)
                    .map(|t| t.trim_start_matches('\u{feff}').trim().to_owned())
                    .map_err(|_| {
                        "文件已保存；文本不是 UTF-8 编码，可补充笔记或转换编码后重试".into()
                    })
            }
        } else if ext == "pdf" {
            crate::wiki_adapters::pdf_extract_text(&snapshot).map_err(|e| e.to_string())
        } else if matches!(ext.as_str(), "html" | "htm" | "xhtml") {
            crate::wiki_adapters::extract_source(&snapshot, text_limit)
                .map(|s| {
                    s.chunks
                        .into_iter()
                        .map(|c| c.text)
                        .collect::<Vec<_>>()
                        .join("\n")
                })
                .map_err(|e| e.to_string())
        } else {
            Err("原件已保存，当前仅检索标题和笔记，尚未解析此格式".into())
        };
        let (body, status, error) = match parsed {
            Ok(text) if text.trim().is_empty() => (
                String::new(),
                if text_format || size == 0 { "empty" } else { "failed" },
                Some(if size == 0 {"文件为空，原件已保存，可添加笔记"} else if text_format {"正文仅含空白，原件已保存，可添加笔记"} else {"未提取到正文，原件已保存；扫描版资料暂不支持文字识别，可先补充笔记"}.to_owned()),
            ),
            Ok(text) => (text, "ready", None),
            Err(e) => (
                String::new(),
                if text_format || ext == "pdf" || matches!(ext.as_str(), "html" | "htm" | "xhtml") {
                    "failed"
                } else {
                    "unsupported"
                },
                Some(e),
            ),
        };
        input.body = body;
        self.save_item_with_asset(
            subject,
            &input,
            Some((&relative, name, status, error.as_deref())),
        )
    }

    pub fn knowledge_item_asset(&self, subject: &str, document: &str) -> Result<PathBuf> {
        let item = self.knowledge_item(subject, document)?;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let relative:Option<String>=conn.query_row("SELECT asset_path FROM knowledge_item_details WHERE document_id=?1 AND version_id=?2",params![document,item.version_id],|r|r.get(0)).optional()?.flatten();
        let relative = relative.ok_or_else(|| Error::Invalid("该条目没有已保存的原件".into()))?;
        let path = self.data_dir.join(&relative).canonicalize()?;
        if !path.starts_with(self.data_dir.join("knowledge/assets").canonicalize()?) {
            return Err(Error::Invalid("附件路径越界".into()));
        }
        Ok(path)
    }

    pub fn knowledge_remove_item(&self, subject: &str, document: &str) -> Result<()> {
        let _assets = self
            .item_assets
            .lock()
            .map_err(|_| Error::Store("lock".into()))?;
        let _write = self
            .item_write
            .lock()
            .map_err(|_| Error::Store("lock".into()))?;
        let item = self.knowledge_item(subject, document)?;
        let space = self
            .knowledge_space_by_id(&item.space_id)?
            .ok_or_else(|| Error::Invalid("知识库不存在".into()))?;
        let assets: Vec<String> = {
            let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
            let mut st=conn.prepare("SELECT DISTINCT a.asset_path FROM knowledge_item_asset_versions a JOIN knowledge_document_versions v ON v.id=a.version_id WHERE v.document_id=?1")?;
            let rows = st
                .query_map([document], |r| r.get(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        self.knowledge_delete_document(
            &space.tenant_id,
            &space.customer_id,
            &space.id,
            subject,
            document,
        )?;
        for relative in assets {
            let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
            let referenced: bool = conn.query_row(
                "SELECT EXISTS(SELECT 1 FROM knowledge_item_asset_versions WHERE asset_path=?1)",
                [&relative],
                |r| r.get(0),
            )?;
            if !referenced {
                let path = self.data_dir.join(&relative);
                if path.starts_with(self.data_dir.join("knowledge/assets")) {
                    let _ = std::fs::remove_file(path);
                }
            }
        }
        Ok(())
    }

    pub fn knowledge_item_set_organization(
        &self,
        subject: &str,
        document: &str,
        version: &str,
        topics: Option<Vec<String>>,
        favorite: Option<bool>,
    ) -> Result<()> {
        let item = self.knowledge_item(subject, document)?;
        self.knowledge_authorize(&item.space_id, subject, KnowledgeRole::Editor)?;
        if item.version_id != version {
            return Err(Error::Invalid("内容已更新，请刷新后操作".into()));
        }
        // Upgrade legacy imported documents into presentation metadata only.
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        conn.execute("INSERT OR IGNORE INTO knowledge_item_details(document_id,version_id,title,kind,body,notes,parse_status,updated_at) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",params![item.id,item.version_id,item.title,item.kind,item.body,item.notes,item.parse_status,item.updated_at])?;
        if let Some(topics) = topics {
            let topics = clean_topics(topics);
            conn.execute("UPDATE knowledge_item_details SET topics=?2,suggested_topics='[]',manual_topics=1,classification_status='complete' WHERE document_id=?1 AND version_id=?3",params![document,serde_json::to_string(&topics)?,version])?;
        }
        if let Some(favorite) = favorite {
            conn.execute("UPDATE knowledge_item_details SET favorite=?2 WHERE document_id=?1 AND version_id=?3",params![document,favorite,version])?;
        }
        Ok(())
    }

    pub fn knowledge_item_classification_result(
        &self,
        subject: &str,
        document: &str,
        version: &str,
        topics: Vec<String>,
        suggestions: Vec<String>,
        error: Option<&str>,
    ) -> Result<bool> {
        let item = self.knowledge_item(subject, document)?;
        self.knowledge_authorize(&item.space_id, subject, KnowledgeRole::Editor)?;
        let conn = self.conn.lock().map_err(|_| Error::Store("lock".into()))?;
        let n=conn.execute("UPDATE knowledge_item_details SET topics=?3,suggested_topics=?4,classification_status=?5,classification_error=?6 WHERE document_id=?1 AND version_id=?2 AND manual_topics=0 AND EXISTS(SELECT 1 FROM knowledge_documents d WHERE d.id=?1 AND d.deleted_at IS NULL)",params![document,version,serde_json::to_string(&clean_topics(topics))?,serde_json::to_string(&clean_topics(suggestions))?,if error.is_some(){"failed"}else{"complete"},error])?;
        Ok(n > 0)
    }
}

fn clean_topics(topics: Vec<String>) -> Vec<String> {
    let mut out = Vec::new();
    for t in topics {
        let t = t.trim();
        if !t.is_empty()
            && t.chars().count() <= 40
            && !out.iter().any(|s: &String| s.eq_ignore_ascii_case(t))
        {
            out.push(t.to_owned());
        }
        if out.len() == 8 {
            break;
        }
    }
    out
}
