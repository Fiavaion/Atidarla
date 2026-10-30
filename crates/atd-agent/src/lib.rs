//! The agent layer (PRD §7.6): typed tools over a project (FR-G1), staged
//! changes for review (FR-G2), Show Lock and permission scopes (FR-G3), and
//! the MCP server that offers the tools to any MCP client (A15). There is
//! no arbitrary code execution outside the opt-in `python.exec` scope
//! (CLAUDE.md rule 9).

pub mod assistant;
pub mod cues;
pub mod lock;
pub mod mcp;
pub mod tools;
pub mod workspace;

pub use lock::{Scope, ShowLock};
pub use tools::{Agent, Output, ToolSpec};
pub use workspace::{NetworkWorkspace, Workspace};
