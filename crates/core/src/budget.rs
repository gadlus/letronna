//! A token budget a whole tree of agents spends from.
//!
//! Each agent's budget names the one it was started under as its parent, so
//! a reply charged anywhere in the tree lands on every ledger above it, and a
//! check refuses while any of them is spent. One runaway branch cannot spend
//! what the rest of the tree was given, and a parent's limit still binds
//! helpers that were given a larger one of their own.
//!
//! The units are tokens rather than money because `gateway.rs` knows no
//! chat-model prices. The check comes before a call and the charge after it,
//! since what a reply costs is only known once it has arrived; so a tree can
//! finish a little past its limit, by at most one reply per agent still
//! running, and the refusal says by how much.

use crate::chat::Message;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

/// Providers bill a prompt-cache hit at about a tenth of fresh input, so a
/// cached token spends a tenth of one here.
const CACHED_SHARE: u64 = 10;

pub struct Budget {
    limit: u64,
    spent: AtomicU64,
    parent: Option<Arc<Budget>>,
}

impl Budget {
    /// A budget of `limit` tokens that also spends from `parent`, if there is
    /// one.
    pub fn new(limit: u64, parent: Option<Arc<Budget>>) -> Self {
        Self {
            limit,
            spent: AtomicU64::new(0),
            parent,
        }
    }

    /// The tokens charged to this budget so far, by this agent and every
    /// agent under it.
    pub fn spent(&self) -> u64 {
        self.spent.load(Ordering::Relaxed)
    }

    /// Charges a reply's fresh input, a tenth of its cached input and its
    /// output, here and to every ancestor.
    pub fn charge(&self, reply: &Message) {
        let fresh = reply.input_tokens().saturating_sub(reply.cached_tokens);
        let cost = fresh + reply.cached_tokens / CACHED_SHARE + reply.tokens;
        for budget in std::iter::successors(Some(self), |budget| budget.parent.as_deref()) {
            budget.spent.fetch_add(cost, Ordering::Relaxed);
        }
    }

    /// The fewest tokens left in this budget or any ancestor, none once one
    /// is spent.
    pub fn left(&self) -> u64 {
        std::iter::successors(Some(self), |budget| budget.parent.as_deref())
            .map(|budget| budget.limit.saturating_sub(budget.spent()))
            .min()
            .unwrap_or_default()
    }

    /// Refuses the next call when this budget or any ancestor is spent,
    /// naming the one that is.
    pub fn check(&self) -> Result<(), String> {
        for budget in std::iter::successors(Some(self), |budget| budget.parent.as_deref()) {
            let spent = budget.spent();
            if spent >= budget.limit {
                return Err(format!(
                    "The helper budget is spent ({} of {} tokens). Finish the work yourself.",
                    short(spent),
                    short(budget.limit)
                ));
            }
        }
        Ok(())
    }
}

/// A token count the way a person says it: 512k, 3.0M, or the count itself
/// below a thousand.
pub fn short(tokens: u64) -> String {
    if tokens < 1_000 {
        tokens.to_string()
    } else if tokens < 1_000_000 {
        format!("{}k", tokens / 1_000)
    } else {
        format!("{}.{}M", tokens / 1_000_000, tokens / 100_000 % 10)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn reply(prompt: u64, cached: u64, output: u64) -> Message {
        Message {
            prompt_tokens: prompt,
            cached_tokens: cached,
            tokens: output,
            ..Default::default()
        }
    }

    #[test]
    fn a_child_spends_from_its_parents_budget() {
        let root = Arc::new(Budget::new(10_000, None));
        let parent = Arc::new(Budget::new(5_000, Some(root.clone())));
        let child = Budget::new(5_000, Some(parent.clone()));
        child.charge(&reply(1_000, 0, 200));
        assert_eq!(child.spent(), 1_200);
        assert_eq!(parent.spent(), 1_200);
        assert_eq!(root.spent(), 1_200);

        let sibling = Budget::new(5_000, Some(parent.clone()));
        sibling.charge(&reply(3_500, 0, 300));
        assert_eq!(child.spent(), 1_200);
        assert_eq!(
            child.check(),
            Err("The helper budget is spent (5k of 5k tokens). Finish the work yourself.".into())
        );
        assert_eq!(root.check(), Ok(()));
    }

    #[test]
    fn what_is_left_is_the_least_any_ledger_above_has() {
        let root = Arc::new(Budget::new(10_000, None));
        let parent = Arc::new(Budget::new(5_000, Some(root.clone())));
        let child = Budget::new(8_000, Some(parent.clone()));
        child.charge(&reply(1_000, 0, 200));
        assert_eq!(child.left(), 3_800);
        assert_eq!(root.left(), 8_800);

        child.charge(&reply(4_000, 0, 0));
        assert_eq!(child.left(), 0);
    }

    #[test]
    fn cached_input_counts_a_tenth() {
        let budget = Budget::new(1_000_000, None);
        budget.charge(&reply(10_000, 9_000, 500));
        assert_eq!(budget.spent(), 1_000 + 900 + 500);

        let counted_beside = Budget::new(1_000_000, None);
        counted_beside.charge(&reply(100, 1_830, 0));
        assert_eq!(counted_beside.spent(), 100 + 183);
    }

    #[test]
    fn spending_past_the_limit_is_refused_before_the_call() {
        let budget = Budget::new(500_000, None);
        budget.charge(&reply(400_000, 0, 50_000));
        assert_eq!(budget.check(), Ok(()));
        budget.charge(&reply(60_000, 0, 2_000));
        assert_eq!(
            budget.check(),
            Err(
                "The helper budget is spent (512k of 500k tokens). Finish the work yourself."
                    .into()
            )
        );
    }
}
