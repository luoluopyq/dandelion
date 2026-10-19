pub const BUILTIN_TOOL_NAMES: &[&str] = &[
    "read_file",
    "write_file",
    "patch",
    "search_files",
    "terminal",
    "process",
    "delegate_task",
    "todo",
    "web_search",
    "web_extract",
    // H3 meta-tool that promotes lazy MCP tools; reserved so a server tool
    // can never shadow the entry point the model uses to discover them.
    "find_tools",
];

pub fn sanitize_mcp_part(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for c in raw.chars() {
        if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
            out.push(c);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() {
        "_".into()
    } else {
        out
    }
}

pub fn mcp_tool_name(server_id: &str, tool_name: &str) -> String {
    format!(
        "mcp__{}__{}",
        sanitize_mcp_part(server_id),
        sanitize_mcp_part(tool_name)
    )
}

pub fn collides_with_builtin(name: &str) -> bool {
    BUILTIN_TOOL_NAMES.iter().any(|b| *b == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prefix_and_sanitize() {
        assert_eq!(mcp_tool_name("echo", "ping"), "mcp__echo__ping");
        assert_eq!(
            mcp_tool_name("my.server", "do thing"),
            "mcp__my_server__do_thing"
        );
        assert!(!collides_with_builtin("mcp__echo__ping"));
        assert!(collides_with_builtin("read_file"));
        assert_eq!(sanitize_mcp_part(""), "_");
        assert_eq!(sanitize_mcp_part("你好"), "__");
        for b in BUILTIN_TOOL_NAMES {
            assert!(collides_with_builtin(b), "{b}");
            assert!(!mcp_tool_name("echo", b).is_empty());
            assert_ne!(mcp_tool_name("echo", b), *b);
        }
    }

    #[test]
    fn keeps_underscore_and_hyphen() {
        assert_eq!(sanitize_mcp_part("a_b-c1"), "a_b-c1");
    }
}
