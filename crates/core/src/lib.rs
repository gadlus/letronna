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
pub mod commit;
pub mod compact;
pub mod complete;
pub mod computer;
mod contract;
#[cfg(feature = "workflows")]
pub mod crew;
pub mod diag;
mod downloads;
pub mod driver;
mod edit;
pub mod failure;
pub mod fallback;
pub mod files;
mod filestate;
pub mod gate;
pub mod gateway;
pub mod git;
pub mod goal;
mod grep;
pub mod headless;
pub mod history;
pub mod hooks;
pub mod import;
pub mod inbox;
pub mod init;
pub mod insights;
pub mod interrupt;
pub mod jobs;
pub mod judge;
pub mod limits;
mod login_env;
pub mod lsp;
pub mod mcp;
pub mod mcp_auth;
mod mcp_find;
pub mod mcp_paste;
pub mod mcp_serve;
pub mod media;
pub mod memory;
pub mod messaging;
pub mod models;
mod notebook;
pub mod notify;
mod page;
pub mod palette;
mod patch;
pub mod paths;
pub mod permissions;
pub mod plugins;
mod prefilter;
pub mod preview;
pub mod profiles;
mod redact;
pub mod references;
pub mod remote;
pub mod retry;
pub mod review;
pub mod sandbox;
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
mod taint;
pub mod tasklist;
pub mod tasks;
mod think;
pub mod timers;
pub mod tool_runs;
pub mod tools;
pub mod toolset;
mod unified;
pub mod untrusted;
pub mod update;
pub mod usage;
pub mod vault;
pub mod wake;
pub mod watch;
pub mod web;
#[cfg(feature = "workflows")]
pub mod workflow;
pub mod worktree;
mod write;

/// The name the app goes by, used in paths and on screen.
pub const BRAND: &str = "Letronna";
