//! The user's rules for what the agent may spend, and the record of what it
//! has spent.
//!
//! Money is counted in millionths of a dollar (micro-dollars): USDC's own
//! smallest unit, so a $0.001 x402 call counts as what it costs rather than
//! rounded up to a cent, which would let a day's cap run out ten times too
//! soon. A token worth a dollar counts as it is; a native coin at a spot
//! price fetched from Coinbase and kept five minutes; anything else has no
//! known value, and nothing without one is paid or sent by the agent, as
//! it could not be held to the caps.
//!
//! Every payment the agent makes is written to the ledger as pending before
//! anything is signed, under the file's lock, after the caps are checked
//! against what the ledger already holds. So the window, the terminal and a
//! helper paying at the same moment cannot pass a cap between them, and a
//! payment whose outcome is unclear -- signed, but the site never said --
//! keeps counting, as the site may still settle it.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The ledger, in the data folder.
pub const LEDGER_FILE: &str = "wallet-ledger.json";
/// Entries older than a month kept past this many, for the history.
const KEPT: usize = 2_000;
/// A day and a month, for the caps, as rolling windows: a burst of payments
/// either side of midnight still meets one day's cap.
const DAY: i64 = 24 * 60 * 60;
const MONTH: i64 = 30 * DAY;
/// How long a coin's dollar price is trusted before it is asked again.
const PRICE_TIME: Duration = Duration::from_secs(5 * 60);
/// Micro-dollars in a dollar.
pub const DOLLAR: u64 = 1_000_000;

/// What the agent does with a site that asks to be paid (x402).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum X402 {
    /// Never pays one.
    Off,
    /// Shows the user each price on a card.
    #[default]
    Ask,
    /// Pays on its own up to `auto_up_to`, within the caps; asks above it.
    Auto,
}

/// The user's spending rules.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Policy {
    /// Nothing is paid or sent by the agent while this is set.
    pub paused: bool,
    pub x402: X402,
    /// The most an x402 payment may cost to be made without asking, in Auto.
    pub auto_up_to: u64,
    /// Caps no payment by the agent goes past, asked or not.
    pub per_payment: u64,
    pub per_day: u64,
    pub per_month: u64,
    /// The agent sends only to addresses saved in the wallet.
    pub saved_only: bool,
    /// When any are listed, the only sites x402 payments go to.
    pub sites_allow: Vec<String>,
    /// Sites never paid.
    pub sites_block: Vec<String>,
    /// Test networks are on (each can still be switched by hand).
    pub testnets: bool,
    /// CAIP-2 ids tried first when a site takes several networks.
    pub prefer: Vec<String>,
}

impl Default for Policy {
    fn default() -> Self {
        Self {
            paused: false,
            x402: X402::Ask,
            auto_up_to: DOLLAR / 10,
            per_payment: DOLLAR,
            per_day: 5 * DOLLAR,
            per_month: 25 * DOLLAR,
            saved_only: false,
            sites_allow: Vec::new(),
            sites_block: Vec::new(),
            testnets: false,
            prefer: vec![super::chains::SOLANA.into(), super::chains::BASE.into()],
        }
    }
}

/// Who made a payment.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    /// The agent paid a site that asked (x402).
    X402,
    /// The agent sent money, with the user's yes.
    Send,
    /// The user sent it by hand, which no cap counts.
    Own,
}

/// How a payment went.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    /// Signed or about to be, the outcome not known yet.
    Pending,
    Paid,
    /// Nothing was signed, or the chain refused it.
    Failed,
    /// Signed and handed over, but not seen settled: the site may still
    /// settle it until the authorization expires, or a send may be on the
    /// chain though no RPC said so, so it keeps counting.
    Unsettled,
}

/// One payment in the ledger.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    pub id: String,
    /// Unix seconds.
    pub at: i64,
    pub kind: Kind,
    pub status: Status,
    /// The network's name.
    pub chain: String,
    /// The token's or coin's symbol.
    pub token: String,
    /// The amount as a person reads it, such as `0.01`.
    pub amount: String,
    /// What it was worth in micro-dollars, when that was known.
    #[serde(default)]
    pub usd: Option<u64>,
    /// Who was paid: an address.
    pub to: String,
    /// The site paid, for x402.
    #[serde(default)]
    pub site: String,
    /// The wallet's name.
    pub wallet: String,
    #[serde(default)]
    pub tx: String,
    #[serde(default)]
    pub link: String,
    /// Why it failed, or what the site said.
    #[serde(default)]
    pub note: String,
}

/// What the agent has spent in the last day and month, in micro-dollars.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Spent {
    pub day: u64,
    pub month: u64,
}

pub fn ledger_path() -> PathBuf {
    crate::chat::data_dir().join(LEDGER_FILE)
}

/// Every payment kept, oldest first.
pub fn ledger() -> Vec<Entry> {
    crate::state::read(&ledger_path()).unwrap_or_default()
}

/// What the agent has spent by `now`: its payments that went or may still
/// go, counted in the windows they fall in.
pub fn spent(entries: &[Entry], now: i64) -> Spent {
    let mut spent = Spent::default();
    for entry in entries {
        let counts = entry.kind != Kind::Own && entry.status != Status::Failed;
        let usd = entry.usd.unwrap_or(0);
        if counts && now - entry.at < MONTH {
            spent.month += usd;
            if now - entry.at < DAY {
                spent.day += usd;
            }
        }
    }
    spent
}

/// Whether `policy`'s caps leave room for a payment worth `usd`, given what
/// is `spent`; the reason when they do not.
pub(crate) fn room(policy: &Policy, spent: Spent, usd: u64) -> Result<(), String> {
    let raise = "The user can raise it in Settings › Wallet or with `letronna wallet limits`.";
    if usd > policy.per_payment {
        return Err(format!(
            "{} is over the {} cap on one payment. {raise}",
            dollars(usd),
            dollars(policy.per_payment)
        ));
    }
    if spent.day + usd > policy.per_day {
        return Err(format!(
            "{} would pass the {} a day cap ({} spent in the last 24 hours). {raise}",
            dollars(usd),
            dollars(policy.per_day),
            dollars(spent.day)
        ));
    }
    if spent.month + usd > policy.per_month {
        return Err(format!(
            "{} would pass the {} a month cap ({} spent in the last 30 days). {raise}",
            dollars(usd),
            dollars(policy.per_month),
            dollars(spent.month)
        ));
    }
    Ok(())
}

/// Write `entry` to the ledger as pending, unless the caps leave no room for
/// it: checked against the ledger as it is on disk, under its lock, so
/// payments made at once cannot pass a cap together. The user's own sends
/// are recorded without a check.
pub(crate) fn reserve(policy: &Policy, entry: Entry) -> Result<(), String> {
    let path = ledger_path();
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| e.to_string())?;
    }
    let mut refused = None;
    crate::state::update::<Vec<Entry>>(&path, |disk| {
        let mut entries: Vec<Entry> = serde_json::from_value(disk.clone()).unwrap_or_default();
        if entry.kind != Kind::Own {
            let Some(usd) = entry.usd else {
                let yourself = match entry.kind {
                    Kind::Send => " The user can send it themselves with `letronna wallet send`.",
                    _ => "",
                };
                refused = Some(format!(
                    "What {} {} is worth in dollars is not known, so it cannot be held to the limits.{yourself}",
                    entry.amount, entry.token
                ));
                return;
            };
            if let Err(why) = room(policy, spent(&entries, entry.at), usd) {
                refused = Some(why);
                return;
            }
        }
        entries.push(entry);
        prune(&mut entries, crate::chat::now() as i64);
        if let Ok(value) = serde_json::to_value(&entries) {
            *disk = value;
        }
    })?;
    refused.map_or(Ok(()), Err)
}

/// Record how the payment `id` went.
pub(crate) fn settle(id: &str, status: Status, tx: &str, link: &str, note: &str) {
    let _ = crate::state::update::<Vec<Entry>>(&ledger_path(), |disk| {
        let Some(entries) = disk.as_array_mut() else {
            return;
        };
        let Some(entry) = entries.iter_mut().find(|e| e["id"] == id) else {
            return;
        };
        entry["status"] = serde_json::to_value(status).unwrap_or(Value::Null);
        entry["tx"] = tx.into();
        entry["link"] = link.into();
        entry["note"] = note.into();
    });
}

/// Drop the oldest entries past `KEPT`, never one inside the month the caps
/// still count.
fn prune(entries: &mut Vec<Entry>, now: i64) {
    while entries.len() > KEPT && entries.first().is_some_and(|e| now - e.at >= MONTH) {
        entries.remove(0);
    }
}

/// Whether an x402 payment worth `usd` to `site` is made without asking.
pub(crate) fn automatic(policy: &Policy, usd: Option<u64>, site: &str) -> bool {
    !policy.paused
        && policy.x402 == X402::Auto
        && usd.is_some_and(|usd| usd <= policy.auto_up_to)
        && site_allowed(policy, site).is_ok()
}

/// Whether x402 payments may go to `site` (a host); the reason when not.
pub(crate) fn site_allowed(policy: &Policy, site: &str) -> Result<(), String> {
    let site = site.trim_end_matches('.').to_lowercase();
    let under = |listed: &String| {
        let listed = listed.trim().trim_start_matches("*.").to_lowercase();
        !listed.is_empty() && (site == listed || site.ends_with(&format!(".{listed}")))
    };
    if policy.sites_block.iter().any(under) {
        return Err(format!("{site} is on the wallet's blocked sites."));
    }
    if !policy.sites_allow.is_empty() && !policy.sites_allow.iter().any(under) {
        return Err(format!(
            "{site} is not one of the sites the wallet pays (Settings › Wallet)."
        ));
    }
    Ok(())
}

/// Micro-dollars as a person reads them: cents, or the digits that matter
/// below a cent.
pub fn dollars(micros: u64) -> String {
    let whole = micros / DOLLAR;
    let rest = micros % DOLLAR;
    if rest.is_multiple_of(10_000) {
        return format!("${whole}.{:02}", rest / 10_000);
    }
    let digits = format!("{rest:06}");
    format!("${whole}.{}", digits.trim_end_matches('0'))
}

/// A dollar amount the user typed, such as `$1.50` or `0.10`, in
/// micro-dollars.
pub fn parse_dollars(text: &str) -> Result<u64, String> {
    let text = text.trim().trim_start_matches('$').replace(',', "");
    let atomic = super::parse_amount(&text, 6)
        .map_err(|_| format!("`{text}` is not an amount of dollars."))?;
    u64::try_from(atomic).map_err(|_| format!("`{text}` is too many dollars."))
}

/// What `atomic` of a coin with `decimals` is worth, in micro-dollars, at a
/// dollar a unit for a dollar token or at `symbol`'s spot price; rounded up,
/// so a limit is never undercounted.
pub(crate) fn worth(atomic: u128, decimals: u8, dollar: bool, symbol: &str) -> Option<u64> {
    let scale = 10u128.checked_pow(decimals as u32)?;
    let price = if dollar {
        DOLLAR as u128
    } else {
        (price_of(symbol)? * DOLLAR as f64) as u128
    };
    let micros = atomic.checked_mul(price)?.div_ceil(scale);
    u64::try_from(micros).ok()
}

/// One coin's dollar price from Coinbase's public prices, kept five minutes.
fn price_of(symbol: &str) -> Option<f64> {
    static PRICES: Mutex<Vec<(String, Instant, f64)>> = Mutex::new(Vec::new());
    let symbol = symbol.to_uppercase();
    if let Ok(prices) = PRICES.lock()
        && let Some((_, _, price)) = prices
            .iter()
            .find(|(s, at, _)| *s == symbol && at.elapsed() < PRICE_TIME)
    {
        return Some(*price);
    }
    let url = format!("https://api.coinbase.com/v2/prices/{symbol}-USD/spot");
    let answer: Value = super::client().ok()?.get(url).send().ok()?.json().ok()?;
    let price: f64 = answer["data"]["amount"].as_str()?.parse().ok()?;
    if !(price.is_finite() && price > 0.0) {
        return None;
    }
    if let Ok(mut prices) = PRICES.lock() {
        prices.retain(|(s, _, _)| *s != symbol);
        prices.push((symbol, Instant::now(), price));
    }
    Some(price)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::{DATA_DIR, Scratch};

    fn entry(id: &str, at: i64, usd: Option<u64>, kind: Kind) -> Entry {
        Entry {
            id: id.into(),
            at,
            kind,
            status: Status::Pending,
            chain: "Base".into(),
            token: "USDC".into(),
            amount: "1".into(),
            usd,
            to: "0x0".into(),
            site: String::new(),
            wallet: "Main".into(),
            tx: String::new(),
            link: String::new(),
            note: String::new(),
        }
    }

    #[test]
    fn money_is_shown_to_the_cent_or_to_what_matters_below_one() {
        assert_eq!(dollars(0), "$0.00");
        assert_eq!(dollars(DOLLAR), "$1.00");
        assert_eq!(dollars(1_500_000), "$1.50");
        assert_eq!(dollars(1_000), "$0.001");
        assert_eq!(dollars(12_345_678), "$12.345678");
        assert_eq!(parse_dollars("$1.50").unwrap(), 1_500_000);
        assert_eq!(parse_dollars("0.001").unwrap(), 1_000);
        assert_eq!(parse_dollars("1,000").unwrap(), 1_000 * DOLLAR);
        assert!(parse_dollars("ten").is_err());
        assert!(parse_dollars("0.0000001").is_err());
    }

    #[test]
    fn a_dollar_token_is_worth_its_amount_and_rounds_up() {
        assert_eq!(worth(10_000, 6, true, "USDC"), Some(10_000));
        // BNB Chain's USDC has 18 decimals.
        assert_eq!(worth(10u128.pow(18), 18, true, "USDC"), Some(DOLLAR));
        assert_eq!(worth(1, 18, true, "USDC"), Some(1));
    }

    #[test]
    fn the_caps_count_the_agents_payments_in_rolling_windows() {
        let now = 10 * MONTH;
        let mut entries = vec![
            entry("a", now - 60, Some(2 * DOLLAR), Kind::X402),
            entry("b", now - 2 * DAY, Some(3 * DOLLAR), Kind::Send),
            entry("c", now - 2 * MONTH, Some(50 * DOLLAR), Kind::X402),
            entry("d", now - 60, Some(40 * DOLLAR), Kind::Own),
        ];
        entries.push(Entry {
            status: Status::Failed,
            ..entry("e", now - 60, Some(9 * DOLLAR), Kind::X402)
        });
        assert_eq!(
            spent(&entries, now),
            Spent {
                day: 2 * DOLLAR,
                month: 5 * DOLLAR
            }
        );
        let policy = Policy::default();
        assert!(room(&policy, spent(&entries, now), DOLLAR / 2).is_ok());
        assert!(
            room(&policy, spent(&entries, now), 2 * DOLLAR)
                .unwrap_err()
                .contains("one payment")
        );
        let busy = Policy {
            per_payment: 10 * DOLLAR,
            ..Policy::default()
        };
        assert!(
            room(&busy, spent(&entries, now), 4 * DOLLAR)
                .unwrap_err()
                .contains("a day")
        );
        let monthly = Policy {
            per_payment: 100 * DOLLAR,
            per_day: 100 * DOLLAR,
            ..Policy::default()
        };
        assert!(
            room(&monthly, spent(&entries, now), 21 * DOLLAR)
                .unwrap_err()
                .contains("a month")
        );
    }

    #[test]
    fn a_reservation_is_refused_once_the_cap_is_met_and_racers_cannot_pass_it() {
        let _turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = Scratch::new("wallet-ledger");
        unsafe { std::env::set_var("AGENT_DATA_DIR", &*dir) };
        let policy = Policy {
            per_day: DOLLAR,
            ..Policy::default()
        };
        let now = crate::chat::now() as i64;
        // Twenty payments of ten cents at once against a dollar a day.
        let made: Vec<bool> = std::thread::scope(|scope| {
            let runs: Vec<_> = (0..20)
                .map(|n| {
                    let policy = &policy;
                    scope.spawn(move || {
                        reserve(
                            policy,
                            entry(&format!("p{n}"), now, Some(DOLLAR / 10), Kind::X402),
                        )
                        .is_ok()
                    })
                })
                .collect();
            runs.into_iter().map(|run| run.join().unwrap()).collect()
        });
        assert_eq!(made.iter().filter(|&&made| made).count(), 10);
        assert_eq!(spent(&ledger(), now).day, DOLLAR);
        // A payment of no known worth is refused, the user's own goes through.
        let unknown = reserve(&policy, entry("u", now, None, Kind::X402)).unwrap_err();
        assert!(unknown.contains("not known"));
        // Nor is a send counted as nothing when its worth is unknown.
        let unknown = reserve(&policy, entry("v", now, None, Kind::Send)).unwrap_err();
        assert!(unknown.contains("send it themselves"), "{unknown}");
        assert!(reserve(&policy, entry("own", now, Some(9 * DOLLAR), Kind::Own)).is_ok());
        // A failed one stops counting: whichever ten got in, the first.
        let first = ledger()[0].id.clone();
        settle(&first, Status::Failed, "", "", "refused");
        assert!(reserve(&policy, entry("x", now, Some(DOLLAR / 10), Kind::X402)).is_ok());
        let found = ledger().into_iter().find(|e| e.id == first).unwrap();
        assert_eq!(
            (found.status, found.note.as_str()),
            (Status::Failed, "refused")
        );
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn only_a_small_payment_to_an_allowed_site_in_auto_goes_unasked() {
        let mut policy = Policy::default();
        assert!(!automatic(&policy, Some(1_000), "api.example.com"));
        policy.x402 = X402::Auto;
        assert!(automatic(&policy, Some(1_000), "api.example.com"));
        assert!(!automatic(&policy, Some(DOLLAR), "api.example.com"));
        assert!(!automatic(&policy, None, "api.example.com"));
        policy.sites_block = vec!["example.com".into()];
        assert!(!automatic(&policy, Some(1_000), "api.example.com"));
        assert!(site_allowed(&policy, "notexample.com").is_ok());
        policy.sites_block.clear();
        policy.sites_allow = vec!["*.paid.dev".into()];
        assert!(site_allowed(&policy, "api.paid.dev").is_ok());
        assert!(site_allowed(&policy, "api.example.com").is_err());
        policy.paused = true;
        assert!(!automatic(&policy, Some(1_000), "api.paid.dev"));
    }
}
