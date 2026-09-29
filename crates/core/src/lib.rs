//! Letronna without a user interface.
//!
//! Everything here runs headless: the chat transport, the tools, background
//! jobs, schedules, skills, the session store, and the agent loop that ties
//! them together. Nothing in this crate knows whether a window or a terminal
//! is driving it, which is the point -- the two frontends share one agent
//! instead of growing two that slowly disagree.

pub mod agent;
pub mod agent_defs;
pub mod auth;
pub mod browser;
pub mod budget;
pub mod cancel;
mod cdp;
pub mod chat;
pub mod command;
pub mod compact;
pub mod diag;
mod edit;
pub mod failure;
pub mod gateway;
pub mod jobs;
pub mod mcp;
pub mod media;
pub mod memory;
pub mod messaging;
pub mod models;
mod page;
pub mod schedule;
pub mod search;
pub mod skills;
mod state;
pub mod store;
pub mod structured;
pub mod timers;
pub mod tools;
pub mod update;
pub mod watch;
pub mod web;

/// The name the app goes by, used in paths and on screen.
pub const BRAND: &str = "Letronna";
