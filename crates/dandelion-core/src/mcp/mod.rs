pub mod catalog;
pub mod names;
pub mod schema;
pub mod scopes;
pub mod selection;
pub mod stdio;

pub use catalog::McpCatalog;
pub use names::{mcp_tool_name, BUILTIN_TOOL_NAMES};
pub use schema::{apply_schema_budget, SchemaCandidate};
pub use scopes::{resolve as resolve_scopes, ScopeDecision};
pub use selection::ToolSelection;
pub use stdio::{McpListItem, McpManager, McpSnapshot};
