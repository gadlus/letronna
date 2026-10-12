//! Your Letronna account: the hosted pool, charged to your credit. Signed in
//! to once per computer (`auth.rs`), so there is nothing to paste. Its base
//! is the account API's, `auth::hosted_base`, which a test run can move.

use super::{AuthKind, Provider, ProviderInfo, Tier};

pub struct Letronna;

impl Provider for Letronna {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            name: crate::gateway::LETRONNA,
            base: "https://api.letronna.ai/v1",
            blurb: "Your Letronna account. Hosted models, charged to your credit. No key.",
            docs: "https://letronna.ai/docs/hosted-models",
            tier: Tier::Credit,
        }
    }

    fn auth(&self) -> AuthKind {
        AuthKind::Account
    }
}
