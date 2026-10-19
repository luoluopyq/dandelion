//! Context overflow: summarize the prefix, keep recent user-started turns, retry.
//!
//! Same `agent_loop` `while`. No new ACP `sessionUpdate` type. Summarizer prompt
//! is original (not copied from Codex).

use super::{complete_streaming, LoopCtx};
use crate::acp::{ContentBlock, SessionUpdate};
use crate::config::Settings;
use crate::ids::MessageId;
use crate::provider::types::ChatMessage;
use crate::store::{CompactTrigger, CompactionInput};
use crate::{Error, Result};

/// Original brief for the overflow summarizer. Not a vendor prompt.
const COMPACT_SYSTEM: &str = "You compress earlier chat into a short brief for a coding agent. \
Keep: user goals, decisions, file paths, commands, errors, unfinished work. \
Drop: chit-chat and duplicated tool dumps. Output the brief only, no preamble.";

pub const COMPACTED_PREFIX: &str = "[compacted history]\n";

pub async fn try_compact(ctx: &LoopCtx, trigger: CompactTrigger) -> Result<()> {
    let (messages, ids) = ctx.store.load_openai_messages_with_ids(&ctx.session_id)?;
    let keep = ctx.settings.compact_keep_turns.max(1) as usize;
    let (prefix, tail) = split_keep_turns(&messages, keep);
    if prefix.is_empty() {
        return Err(Error::Provider(
            "context overflow: nothing older than keepTurns to compact".into(),
        ));
    }
    let (covered_ids, tail_ids) = ids.split_at(prefix.len());
    // Same text the summarizer has always received; the full length and the
    // truncation flag are only recorded on the checkpoint.
    let full_transcript = format_transcript(&prefix, usize::MAX);
    let source_bytes = full_transcript.len();
    let max_bytes = ctx.settings.compact_source_max_bytes;
    let source_truncated = source_bytes > max_bytes;
    let transcript = truncate_bytes(&full_transcript, max_bytes);
    let compact_id = MessageId::new();
    let mut saw_reasoning = false;
    let summary = complete_streaming(
        ctx,
        COMPACT_SYSTEM.into(),
        vec![ChatMessage::User {
            content: transcript,
            images: Vec::new(),
        }],
        &[],
        &compact_id,
        None,
        false,
        ctx.thinking_intensity.reasoning_effort(),
        &mut saw_reasoning,
    )
    .await?;
    let brief = summary.text.trim().to_string();
    if brief.is_empty() {
        return Err(Error::Provider(
            "context overflow: empty compact brief".into(),
        ));
    }
    let mut todo_segment = None;
    if let Ok(items) = ctx.store.list_todos(&ctx.session_id) {
        if let Some(seg) = crate::tools::todo::format_for_injection(&items) {
            let marker = crate::tools::todo::INJECTION_MARK;
            let already = brief.contains(marker)
                || tail.iter().any(|m| {
                    matches!(m, ChatMessage::User { content, .. } if content.contains(marker))
                });
            if !already {
                todo_segment = Some(seg);
            }
        }
    }
    ctx.store.compact_messages(&CompactionInput {
        session_id: &ctx.session_id,
        covered_ids,
        tail_ids,
        summary: &brief,
        todo: todo_segment.as_deref(),
        trigger,
        source_bytes,
        source_truncated,
    })?;
    (ctx.emit)(SessionUpdate::AgentMessageChunk {
        message_id: MessageId::new(),
        content: ContentBlock::text("Compacted earlier messages."),
    });
    Ok(())
}

/// ~4 UTF-8 bytes per token. No tiktoken.
pub fn estimate_tokens(text: &str) -> u64 {
    (text.len() as u64).div_ceil(4)
}

/// Unknown model window: `max(8 × maxTokens, 32768)` so default 8192 → 64k.
pub fn heuristic_context_window(max_tokens: u32) -> u64 {
    (max_tokens as u64).saturating_mul(8).max(32_768)
}

pub fn prompt_token_budget(max_tokens: u32, reserve_tokens: u32) -> u64 {
    heuristic_context_window(max_tokens)
        .saturating_sub(max_tokens as u64)
        .saturating_sub(reserve_tokens as u64)
}

pub fn estimated_prompt_tokens(
    system: &str,
    messages: &[ChatMessage],
    tools: &[serde_json::Value],
) -> u64 {
    let tools_len = serde_json::to_vec(tools).map(|b| b.len()).unwrap_or(0);
    estimate_tokens(system)
        + estimate_tokens(&format_transcript(messages, usize::MAX))
        + (tools_len as u64).div_ceil(4)
}

/// Same `while` as overflow compact. Skips when there is no prefix to summarize.
pub fn should_preflight(
    settings: &Settings,
    max_tokens: u32,
    system: &str,
    messages: &[ChatMessage],
    tools: &[serde_json::Value],
) -> bool {
    if !settings.compact_enabled || !settings.compact_preflight_enabled {
        return false;
    }
    let keep = settings.compact_keep_turns.max(1) as usize;
    let (prefix, _) = split_keep_turns(messages, keep);
    if prefix.is_empty() {
        return false;
    }
    estimated_prompt_tokens(system, messages, tools)
        > prompt_token_budget(max_tokens, settings.compact_preflight_reserve_tokens)
}

/// Last `keep_turns` user-started turns stay; everything before the cut is prefix.
/// A turn starts at a `User` row and includes following assistant/tool rows.
pub fn split_keep_turns(
    messages: &[ChatMessage],
    keep_turns: usize,
) -> (Vec<ChatMessage>, Vec<ChatMessage>) {
    let keep = keep_turns.max(1);
    let user_idx: Vec<usize> = messages
        .iter()
        .enumerate()
        .filter(|(_, m)| matches!(m, ChatMessage::User { .. }))
        .map(|(i, _)| i)
        .collect();
    if user_idx.len() <= keep {
        return (Vec::new(), messages.to_vec());
    }
    let cut = user_idx[user_idx.len() - keep];
    (messages[..cut].to_vec(), messages[cut..].to_vec())
}

pub fn format_transcript(messages: &[ChatMessage], max_bytes: usize) -> String {
    let mut s = String::new();
    for m in messages {
        match m {
            ChatMessage::User { content, .. } => {
                s.push_str("User: ");
                s.push_str(content);
                s.push('\n');
            }
            ChatMessage::Assistant {
                content,
                tool_calls,
            } => {
                s.push_str("Assistant: ");
                s.push_str(content.as_deref().unwrap_or(""));
                s.push('\n');
                for call in tool_calls {
                    s.push_str("  tool_call ");
                    s.push_str(&call.id);
                    s.push(' ');
                    s.push_str(&call.name);
                    s.push(' ');
                    s.push_str(&call.arguments);
                    s.push('\n');
                }
            }
            ChatMessage::Tool {
                tool_call_id,
                content,
            } => {
                s.push_str("Tool ");
                s.push_str(tool_call_id);
                s.push_str(": ");
                s.push_str(content);
                s.push('\n');
            }
        }
    }
    truncate_bytes(&s, max_bytes)
}

fn truncate_bytes(s: &str, max: usize) -> String {
    if s.len() <= max {
        return s.to_string();
    }
    let mut end = max;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = s[..end].to_string();
    out.push('…');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::provider::types::ToolCall;

    fn user(s: &str) -> ChatMessage {
        ChatMessage::User {
            content: s.into(),
            images: Vec::new(),
        }
    }

    fn assistant(s: &str) -> ChatMessage {
        ChatMessage::Assistant {
            content: Some(s.into()),
            tool_calls: Vec::new(),
        }
    }

    #[test]
    fn keep_one_user_turn_moves_older_to_prefix() {
        let msgs = vec![
            user("hello"),
            assistant("hi"),
            user("next"),
            assistant("ok"),
        ];
        let (prefix, tail) = split_keep_turns(&msgs, 1);
        assert_eq!(prefix.len(), 2);
        assert_eq!(tail.len(), 2);
        match &tail[0] {
            ChatMessage::User { content, .. } => assert_eq!(content, "next"),
            _ => panic!("expected current user"),
        }
    }

    #[test]
    fn keep_turns_preserves_tool_pairing_in_tail() {
        let msgs = vec![
            user("old"),
            assistant("a"),
            user("now"),
            ChatMessage::Assistant {
                content: None,
                tool_calls: vec![ToolCall {
                    id: "c1".into(),
                    name: "read_file".into(),
                    arguments: r#"{"path":"a.rs"}"#.into(),
                }],
            },
            ChatMessage::Tool {
                tool_call_id: "c1".into(),
                content: "fn main".into(),
            },
        ];
        let (prefix, tail) = split_keep_turns(&msgs, 1);
        assert_eq!(prefix.len(), 2);
        assert_eq!(tail.len(), 3);
        assert!(matches!(tail[1], ChatMessage::Assistant { .. }));
        assert!(matches!(tail[2], ChatMessage::Tool { .. }));
    }

    #[test]
    fn too_few_user_turns_is_empty_prefix() {
        let msgs = vec![user("only"), assistant("x")];
        let (prefix, tail) = split_keep_turns(&msgs, 4);
        assert!(prefix.is_empty());
        assert_eq!(tail.len(), 2);
    }

    #[test]
    fn transcript_respects_byte_cap() {
        let msgs = vec![user("abcdefghij")];
        let full = format_transcript(&msgs, 1_000_000);
        let t = format_transcript(&msgs, 8);
        assert!(t.len() < full.len(), "{t:?} vs {full:?}");
        assert!(t.ends_with('…'));
    }

    #[test]
    fn preflight_triggers_when_over_budget_and_prefix_exists() {
        let mut settings = Settings::defaults();
        settings.compact_enabled = true;
        settings.compact_preflight_enabled = true;
        settings.compact_keep_turns = 1;
        settings.compact_preflight_reserve_tokens = 60_000;
        settings.max_tokens = 8192;
        let msgs = vec![user("hello"), assistant("hi"), user("next")];
        assert!(should_preflight(
            &settings,
            settings.max_tokens,
            "sys",
            &msgs,
            &[]
        ));
        settings.compact_preflight_enabled = false;
        assert!(!should_preflight(&settings, 8192, "sys", &msgs, &[]));
        settings.compact_preflight_enabled = true;
        settings.compact_enabled = false;
        assert!(!should_preflight(&settings, 8192, "sys", &msgs, &[]));
    }

    #[test]
    fn preflight_skips_when_nothing_older_than_keep_turns() {
        let mut settings = Settings::defaults();
        settings.compact_preflight_enabled = true;
        settings.compact_keep_turns = 4;
        settings.compact_preflight_reserve_tokens = 60_000;
        let msgs = vec![user("only"), assistant("x")];
        assert!(!should_preflight(&settings, 8192, "sys", &msgs, &[]));
    }
}
