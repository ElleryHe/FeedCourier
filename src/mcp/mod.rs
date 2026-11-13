//! Built-in Model Context Protocol (`/mcp`) Streamable HTTP endpoint, dual-scope Bearer token
//! guard (`readonly` / `admin`), AGPLv3 `ServerInfo` metadata, and untrusted feed content framing.
//!
//! Implemented in Wave 2 (`feedcourier.md` Section 六).

use rmcp as _;
use schemars as _;

/// Canonical HTTP mount path for the Streamable HTTP MCP server.
pub const MCP_MOUNT_PATH: &str = "/mcp";
