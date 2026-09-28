//! Letronna without a user interface.
//!
//! Everything here runs headless: the chat transport, the tools, background
//! jobs, schedules, skills, the session store, and the agent loop that ties
//! them together. Nothing in this crate knows whether a window or a terminal
//! is driving it, which is the point -- the two frontends share one agent
//! instead of growing two that slowly disagree.

pub mod agent;
pub mod approvals;
pub mod auth;
pub mod browser;
pub mod cancel;
mod cdp;
pub mod chat;
pub mod compact;
pub mod gateway;
pub mod inbox;
pub mod jobs;
pub mod mcp;
pub mod media;
pub mod memory;
pub mod messaging;
mod page;
pub mod schedule;
pub mod search;
pub mod skills;
pub mod store;
pub mod timers;
pub mod tools;
pub mod update;
pub mod watch;
pub mod web;

/// The name the app goes by, used in paths and on screen.
pub const BRAND: &str = "Letronna";
