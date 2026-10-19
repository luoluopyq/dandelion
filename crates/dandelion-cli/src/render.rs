use crate::cli::OutputFormat;
use dandelion_core::acp::{ContentBlock, SessionState, SessionUpdate};
use dandelion_core::bus::BusFrame;
use std::collections::HashMap;
use std::io::{self, Write};

#[derive(Default)]
pub struct TextRenderState {
    in_agent_text: bool,
    tool_titles: HashMap<String, String>,
}

pub fn print_frame(
    format: OutputFormat,
    frame: &BusFrame,
    state: &mut TextRenderState,
) -> io::Result<()> {
    match format {
        OutputFormat::Jsonl => {
            let line = serde_json::to_string(frame).expect("bus frame is always serializable");
            let mut out = io::stdout().lock();
            writeln!(out, "{line}")
        }
        OutputFormat::Text => {
            let mut out = io::stdout().lock();
            print_text(frame, state, &mut out)
        }
    }
}

pub fn finish_text(format: OutputFormat, state: &mut TextRenderState) -> io::Result<()> {
    if matches!(format, OutputFormat::Text) {
        let mut out = io::stdout().lock();
        flush_agent_line(state, &mut out)?;
    }
    Ok(())
}

fn print_text(
    frame: &BusFrame,
    state: &mut TextRenderState,
    out: &mut impl Write,
) -> io::Result<()> {
    match &frame.params.update {
        SessionUpdate::StateUpdate {
            state: session,
            stop_reason,
        } => {
            flush_agent_line(state, out)?;
            match session {
                SessionState::Running => {
                    writeln!(out, "… running  session={}", frame.params.session_id)
                }
                SessionState::Queued => {
                    writeln!(out, "… queued   session={}", frame.params.session_id)
                }
                SessionState::Idle => writeln!(
                    out,
                    "✔ idle     session={}  stop={}",
                    frame.params.session_id,
                    stop_reason.as_deref().unwrap_or("-")
                ),
            }
        }
        SessionUpdate::UserMessageChunk { content, .. } => {
            flush_agent_line(state, out)?;
            writeln!(out, "you: {}", content_text(content))
        }
        SessionUpdate::AgentMessageChunk { content, .. } => {
            if !state.in_agent_text {
                write!(out, "dln: ")?;
                state.in_agent_text = true;
            }
            write!(out, "{}", content_text(content))?;
            out.flush()
        }
        // Reasoning is a separate host extension, not assistant body text.
        SessionUpdate::ReasoningDelta { .. } => Ok(()),
        // Image preview paths belong to the desktop renderer; the CLI already
        // printed the user text and has no way to show images.
        SessionUpdate::UserImages { .. } => Ok(()),
        SessionUpdate::ToolCallProgress { .. } => Ok(()),
        SessionUpdate::ToolCallUpdate { update } => {
            flush_agent_line(state, out)?;
            let id = update.tool_call_id.to_string();
            if let Some(title) = &update.title {
                state.tool_titles.insert(id.clone(), title.clone());
            }
            let title = update
                .title
                .as_deref()
                .or_else(|| state.tool_titles.get(&id).map(String::as_str))
                .unwrap_or("tool");
            let status = update.status.map(|s| s.as_str()).unwrap_or("-");
            let extra = update
                .content
                .as_ref()
                .and_then(|c| c.first())
                .map(|c| match c {
                    dandelion_core::acp::ToolCallContent::Content { content } => {
                        format!("  {}", content_text(content))
                    }
                    dandelion_core::acp::ToolCallContent::Terminal { terminal_id } => {
                        format!("  terminal={terminal_id}")
                    }
                })
                .unwrap_or_default();
            writeln!(out, "tool: {title}  id={id}  {status}{extra}")
        }
        SessionUpdate::JobUpdate {
            job_id,
            child_session_id,
            phase,
        } => {
            flush_agent_line(state, out)?;
            writeln!(
                out,
                "job: {phase}  id={job_id}  child={}",
                child_session_id.as_deref().unwrap_or("-")
            )
        }
        SessionUpdate::TerminalUpdate {
            terminal_id,
            command,
            exit_status,
            ..
        } => {
            flush_agent_line(state, out)?;
            if let Some(exit) = exit_status {
                writeln!(
                    out,
                    "term: {terminal_id}  exit={}",
                    exit.exit_code
                        .map(|c| c.to_string())
                        .unwrap_or_else(|| "-".into())
                )
            } else {
                writeln!(
                    out,
                    "term: {terminal_id}  cmd={}",
                    command.as_deref().unwrap_or("-")
                )
            }
        }
        SessionUpdate::TerminalOutputChunk { .. } => Ok(()),
        SessionUpdate::WikiDraft { draft_id, kind, .. } => {
            flush_agent_line(state, out)?;
            writeln!(out, "wiki-draft {kind}  草稿 {draft_id}  待选择项目后提交")
        }
        SessionUpdate::WikiUpdate {
            phase,
            version,
            message,
            ..
        } => {
            flush_agent_line(state, out)?;
            writeln!(
                out,
                "wiki {phase}  v={}  {}",
                version.map(|v| v.to_string()).unwrap_or_else(|| "-".into()),
                message.as_deref().unwrap_or("")
            )
        }
    }
}

fn flush_agent_line(state: &mut TextRenderState, out: &mut impl Write) -> io::Result<()> {
    if state.in_agent_text {
        writeln!(out)?;
        state.in_agent_text = false;
    }
    Ok(())
}

fn content_text(block: &ContentBlock) -> &str {
    match block {
        ContentBlock::Text { text } => text,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dandelion_core::acp::{SessionNotification, ToolCallStatus, ToolCallUpdate};
    use dandelion_core::ids::{MessageId, SessionId, ToolCallId};

    fn frame(update: SessionUpdate) -> BusFrame {
        BusFrame::session_update(
            1,
            SessionNotification {
                session_id: SessionId::parse("tsk_1").unwrap(),
                update,
            },
        )
    }

    #[test]
    fn coalesces_agent_tokens_then_newlines_on_tool() {
        let mut state = TextRenderState::default();
        let mut buf = Vec::new();
        print_text(
            &frame(SessionUpdate::AgentMessageChunk {
                message_id: MessageId::new(),
                content: ContentBlock::text("Hel"),
            }),
            &mut state,
            &mut buf,
        )
        .unwrap();
        print_text(
            &frame(SessionUpdate::AgentMessageChunk {
                message_id: MessageId::new(),
                content: ContentBlock::text("lo"),
            }),
            &mut state,
            &mut buf,
        )
        .unwrap();
        print_text(
            &frame(SessionUpdate::tool_call(ToolCallUpdate {
                tool_call_id: ToolCallId::parse("call_1"),
                title: Some("read_file".into()),
                kind: None,
                status: Some(ToolCallStatus::InProgress),
                raw_input: None,
                content: None,
            })),
            &mut state,
            &mut buf,
        )
        .unwrap();
        print_text(
            &frame(SessionUpdate::tool_call(ToolCallUpdate {
                tool_call_id: ToolCallId::parse("call_1"),
                title: None,
                kind: None,
                status: Some(ToolCallStatus::Completed),
                raw_input: None,
                content: None,
            })),
            &mut state,
            &mut buf,
        )
        .unwrap();
        let s = String::from_utf8(buf).unwrap();
        assert!(s.starts_with("dln: Hello\n"), "{s:?}");
        assert!(s.contains("tool: read_file  id=call_1  in_progress"), "{s}");
        assert!(s.contains("tool: read_file  id=call_1  completed"), "{s}");
        assert!(!s.contains("inprogress"), "{s}");
    }
}
