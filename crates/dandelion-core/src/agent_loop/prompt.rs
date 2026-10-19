use crate::sandbox::WorkspaceFs;
use std::path::Path;

pub const DEFAULT_IDENTITY: &str =
    "你是蒲公英本地 Agent。先理解用户目标；只有需要工作区、外部信息或执行动作时才使用工具。";

#[derive(Clone, Debug, Default)]
pub struct AssembleOpts<'a> {
    pub identity: Option<&'a str>,
    pub memory: &'a str,
    pub context_pack: &'a str,
    pub child_goal: Option<&'a str>,
    pub child_context: Option<&'a str>,
    /// User-authored task prompt. Assembled only in core.
    pub user_prompt: Option<&'a str>,
    pub spec: &'a str,
    pub plan: &'a str,
}

pub fn assemble_system_prompt(
    workspace: &Path,
    tool_names: &[String],
    project: &str,
    skills: &str,
) -> String {
    assemble_system_prompt_ex(
        workspace,
        tool_names,
        project,
        skills,
        AssembleOpts::default(),
    )
}

pub fn assemble_system_prompt_ex(
    workspace: &Path,
    tool_names: &[String],
    project: &str,
    skills: &str,
    opts: AssembleOpts<'_>,
) -> String {
    let names = tool_names.join(", ");
    let identity = opts
        .identity
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or(DEFAULT_IDENTITY);
    let workspace = format!(
        "工作区绝对路径：{}。相对路径都相对这个目录。",
        workspace.display()
    );
    let tools = if tool_names.is_empty() {
        "本轮没有可调用工具。请直接基于已有对话回答；不要编造已读取文件、已执行命令或外部事实。"
            .to_string()
    } else {
        format!(
            "只能用下列工具：{names}。工具不是必选项：已有信息足以回答时直接回答。需要读/改/搜文件时使用 read_file、write_file、patch、search_files；只有明确需要运行命令时才使用 terminal。"
        )
    };
    let safety = "不要逃出工作区。不要改系统目录（例如 C:\\Windows、C:\\Program Files）。不要用工作区工具写产品 data_dir。";
    // 防止"新消息触发旧任务续跑"：历史尾部可能停在被打断/失败任务的半截
    // 工具调用上，弱模型会把用户下一条普通消息读成"继续干"的信号。
    let turn_scope = "本轮任务只来自用户最新一条消息：历史里出现过的旧任务——包括被打断、出错或没做完的轮次——只要用户本轮没有再次提出，就不要继续、补全或\u{201c}顺手收尾\u{201d}；最新消息与旧任务无关（如打招呼、闲聊）时，直接简短回应并等待指示。";
    let mut parts = vec![identity.to_string(), workspace];
    if !opts.memory.trim().is_empty() {
        parts.push(opts.memory.trim().to_string());
    }
    if !opts.context_pack.trim().is_empty() {
        parts.push(opts.context_pack.trim().to_string());
    }
    if !project.trim().is_empty() {
        parts.push(project.trim().to_string());
    }
    if !skills.trim().is_empty() {
        parts.push(skills.trim().to_string());
    }
    if !opts.spec.trim().is_empty() {
        parts.push(opts.spec.trim().to_string());
    }
    if !opts.plan.trim().is_empty() {
        parts.push(opts.plan.trim().to_string());
    }
    if let Some(user) = opts.user_prompt.map(str::trim).filter(|s| !s.is_empty()) {
        parts.push(format!("任务说明：\n{user}"));
    }
    if let Some(goal) = opts.child_goal {
        let mut child = String::from(
            "你是子代理，不是父会话。完成任务后用一段话总结：做了什么、改了哪些文件、还有什么问题。不要把工具调用流水账当作总结。不要再调用工具。",
        );
        child.push_str("\n\nGoal: ");
        child.push_str(goal.trim());
        if let Some(ctx) = opts.child_context {
            if !ctx.trim().is_empty() {
                child.push_str("\n\nContext:\n");
                child.push_str(ctx.trim());
            }
        }
        parts.push(child);
    }
    if tool_names.iter().any(|name|name=="wiki_propose") {
        parts.push("用户要求把整理的知识保存、学习到知识库或更新 Wiki 时，必须调用 wiki_propose，明确要求保存时使用 mode=write。一般学习知识可按概念拆成 term、按步骤拆成 flow。只调用 knowledge_search 不会保存任何内容。以工具返回为准：saved_knowledge 表示已保存为独立知识，无需项目；project_required 表示仅有草稿，可直接保存为独立知识，只有用户需要项目文档时才选择项目；pending 必须告知待审核，accepted 表示项目知识已生效；禁止把写在回复中的正文说成已存入知识库。".into());
    }
    parts.push(tools);
    parts.push(turn_scope.to_string());
    parts.push(safety.to_string());
    parts.join("\n\n")
}

pub fn assemble_for_fs(
    fs: &WorkspaceFs,
    tool_names: &[String],
    project: &str,
    skills: &str,
) -> String {
    assemble_system_prompt(fs.root(), tool_names, project, skills)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn omits_empty_optional_segments() {
        let s = assemble_system_prompt(&PathBuf::from("/ws"), &["read_file".into()], "", "");
        assert!(s.contains("read_file"));
        assert!(!s.contains("项目须知"));
        assert!(!s.contains("已启用技能全文"));
        assert!(s.contains(DEFAULT_IDENTITY));
    }

    #[test]
    fn includes_turn_scope_clause_after_tools_before_safety() {
        let s = assemble_system_prompt(&PathBuf::from("/ws"), &["read_file".into()], "", "");
        let t = s.find("只能用下列工具").unwrap();
        let u = s.find("本轮任务只来自用户最新一条消息").unwrap();
        let a = s.find("不要逃出工作区").unwrap();
        assert!(t < u && u < a);
    }

    #[test]
    fn includes_project_then_skills() {
        let s = assemble_system_prompt(
            &PathBuf::from("/ws"),
            &["read_file".into()],
            "项目须知：\nhello-project",
            "已启用技能全文：\ndemo-skill",
        );
        let p = s.find("hello-project").unwrap();
        let k = s.find("demo-skill").unwrap();
        let t = s.find("只能用下列工具").unwrap();
        assert!(p < k && k < t);
    }

    #[test]
    fn soul_replaces_identity_and_child_outro() {
        let s = assemble_system_prompt_ex(
            &PathBuf::from("/ws"),
            &["read_file".into()],
            "",
            "",
            AssembleOpts {
                identity: Some("SOUL_UNIQUE"),
                memory: "用户画像：\nAda",
                context_pack: "",
                child_goal: Some("fix tests"),
                child_context: Some("crate x"),
                user_prompt: None,
                spec: "",
                plan: "",
            },
        );
        assert!(s.contains("SOUL_UNIQUE"));
        assert!(!s.contains(DEFAULT_IDENTITY));
        assert!(s.contains("Ada"));
        assert!(s.contains("子代理"));
        assert!(s.contains("fix tests"));
        let soul = s.find("SOUL_UNIQUE").unwrap();
        let mem = s.find("Ada").unwrap();
        let child = s.find("子代理").unwrap();
        assert!(soul < mem && mem < child);
    }

    #[test]
    fn user_prompt_sits_after_skills_before_tools() {
        let s = assemble_system_prompt_ex(
            &PathBuf::from("/ws"),
            &["read_file".into()],
            "",
            "已启用技能全文：\ndemo-skill",
            AssembleOpts {
                user_prompt: Some("TASK_PROMPT_UNIQUE"),
                ..AssembleOpts::default()
            },
        );
        let k = s.find("demo-skill").unwrap();
        let u = s.find("TASK_PROMPT_UNIQUE").unwrap();
        let t = s.find("只能用下列工具").unwrap();
        assert!(k < u && u < t);
        assert!(s.contains("任务说明："));
    }
}
