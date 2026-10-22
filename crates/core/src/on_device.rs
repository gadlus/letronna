//! Models that run on this computer: downloaded once, then answering with
//! no internet and nothing sent anywhere.
//!
//! The catalog names each model's files by commit, size and SHA-256. Before
//! any download, this computer is looked at (`machine`) and each model given
//! a verdict and a speed estimate (`fit`), so the picker can say "Runs well"
//! or "Needs 19 GB" up front and suggest the one model that suits it best.
//! The models run under llama.cpp's own server, a pinned build fetched the
//! first time one is wanted, rather than linked in: it brings every backend
//! (Metal, CUDA, Vulkan, the processor) without a build for each, a crash or
//! an out-of-memory ends that process and not the app, and taking a newer
//! llama.cpp is a change of pin.

mod catalog;
mod fit;
mod machine;
pub mod runtime;
pub mod shelf;

pub use catalog::{
    Asset, BUILD, CATALOG, Entry, Pack, RUNTIMES, Runtime, SAMPLING, Sampling, Thinking, Tools,
    entry, runtime,
};
pub use fit::{
    AGENT_CONTEXT, CONTEXT, Fit, LEAST_CONTEXT, ON_BATTERY, Verdict, fit, fits, memory, size,
    speed, suggest,
};
pub use machine::{
    Backend, Choice, Device, Gpu, Kind, Machine, NO_VULKAN, OLD_DRIVER, Vendor, cached, detect,
    device_for, now, parse_devices, pick, ram_free,
};
