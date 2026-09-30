//! Letronna without a user interface.
//!
//! Everything here runs headless: the chat transport, the tools, background
//! jobs, schedules, skills, the session store, and the agent loop that ties
//! them together. Nothing in this crate knows whether a window or a terminal
//! is driving it, which is the point -- the two frontends share one agent
//! instead of growing two that slowly disagree.

pub mod agent;
pub mod agent_defs;
pub mod agents;
pub mod approvals;
pub mod auth;
#[cfg(test)]
mod bench;
pub mod browser;
pub mod budget;
pub mod cache_miss;
pub mod cancel;
mod cdp;
pub mod chat;
pub mod command;
pub mod compact;
pub mod computer;
pub mod diag;
mod downloads;
pub mod driver;
mod edit;
pub mod failure;
pub mod fallback;
mod filestate;
pub mod gate;
pub mod gateway;
pub mod git;
pub mod goal;
mod grep;
pub mod history;
pub mod hooks;
pub mod inbox;
pub mod init;
pub mod insights;
pub mod jobs;
pub mod judge;
pub mod limits;
mod login_env;
pub mod lsp;
pub mod mcp;
mod mcp_find;
pub mod mcp_serve;
pub mod media;
pub mod memory;
pub mod messaging;
pub mod models;
mod notebook;
pub mod notify;
mod page;
mod patch;
pub mod paths;
pub mod permissions;
pub mod preview;
mod redact;
pub mod references;
pub mod retry;
mod scan;
pub mod schedule;
pub mod search;
pub mod skills;
pub mod slash;
pub mod speech;
mod state;
pub mod store;
pub mod structured;
pub mod styles;
mod subfolders;
pub mod tasklist;
pub mod tasks;
mod think;
pub mod timers;
pub mod tools;
pub mod untrusted;
pub mod update;
pub mod usage;
pub mod vault;
pub mod wake;
pub mod watch;
pub mod web;
#[cfg(feature = "workflows")]
pub mod workflow;
mod write;

/// The name the app goes by, used in paths and on screen.
pub const BRAND: &str = "Letronna";
