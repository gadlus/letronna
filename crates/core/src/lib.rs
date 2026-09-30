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
pub mod browser;
pub mod budget;
pub mod cancel;
mod cdp;
pub mod chat;
pub mod command;
pub mod compact;
pub mod diag;
pub mod driver;
mod edit;
pub mod failure;
mod filestate;
pub mod gate;
pub mod gateway;
mod grep;
pub mod history;
pub mod hooks;
pub mod inbox;
pub mod init;
pub mod jobs;
pub mod judge;
pub mod limits;
mod login_env;
pub mod mcp;
mod mcp_find;
pub mod media;
pub mod memory;
pub mod messaging;
pub mod models;
mod page;
pub mod paths;
pub mod permissions;
mod redact;
pub mod retry;
mod scan;
pub mod schedule;
pub mod search;
pub mod skills;
mod state;
pub mod store;
pub mod structured;
pub mod tasklist;
pub mod tasks;
pub mod timers;
pub mod tools;
pub mod untrusted;
pub mod update;
pub mod usage;
pub mod wake;
pub mod watch;
pub mod web;
#[cfg(feature = "workflows")]
pub mod workflow;
pub mod worktree;
mod write;

/// The name the app goes by, used in paths and on screen.
pub const BRAND: &str = "Letronna";
