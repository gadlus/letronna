//! The folder to copy for a new provider (docs/providers.md has the steps).
//!
//! 1. Copy `_template/` to a folder named for the provider, in lowercase.
//! 2. Fill in `info` and pick `auth` and `wire` below.
//! 3. Add `mod <folder>;` and one line to `PROVIDERS` in `providers/mod.rs`.
//! 4. Run `scripts/x.sh test providers`: the shared suite checks it against
//!    the recorded replies in `providers/fixtures/`, no key needed.
//!
//! Override `models` only if the provider lists its models somewhere other
//! than `GET <base>/models`. This file is built only for tests, so it is
//! always checked by the suite and never offered.

use super::{AuthKind, Provider, ProviderInfo, Tier};

pub struct Template;

impl Provider for Template {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: "Template",
            base: "https://api.example.com/v1",
            blurb: "One line on what it is and what it needs.",
            docs: "https://example.com/keys",
            tier: Tier::ApiKey,
        }
    }

    fn auth(&self) -> AuthKind {
        AuthKind::ApiKey
    }
}
