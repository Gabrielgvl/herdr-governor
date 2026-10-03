//! `mcp` — the hand-rolled MCP layer (spec §6.2, plan §4.11, OQ-A): a
//! JSON-RPC 2.0 subset (`jsonrpc`), the relay↔daemon caller-envelope
//! framing under the shared 1 MiB line bound (`framing`), the three
//! tools' strict argument DTOs beside their hand-written `inputSchema`s
//! (`schema`), the `tools/list`/`tools/call` mapping onto `daemon::api`
//! (`tools`), and the socket's accept loop + per-connection task
//! (`serve`).

pub mod framing;
pub mod jsonrpc;
mod schema;
pub mod serve;
pub mod tools;
