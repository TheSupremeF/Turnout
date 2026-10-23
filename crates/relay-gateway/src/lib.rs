//! Turnout Gateway: a loopback-only HTTP server that lets a client speak one
//! protocol while the upstream provider speaks another.
//!
//! - `POST /v1/messages`          Anthropic Messages in (Claude Code)
//! - `POST /v1/chat/completions`  OpenAI Chat in (Codex, OpenCode)
//! - `GET  /v1/models`, `POST /v1/messages/count_tokens`
//!
//! Safety properties: binds `127.0.0.1` only, requires a per-session token,
//! never logs prompts, preserves upstream status codes, and cancels the
//! upstream request as soon as the client disconnects.

pub mod convert;
mod request_log;
mod server;

pub use request_log::{requests_dir, RequestMeter};
pub use server::{Gateway, GatewayConfig, GatewayHandle, GatewayModelRoute};
