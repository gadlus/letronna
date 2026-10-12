//! The wallet: keys the agent can pay with, the user's rules for what it may
//! spend, and the tools that use them.
//!
//! A wallet is made here or brought from another app by its recovery phrase
//! or a key; one phrase holds an EVM address, good on every EVM chain, and a
//! Solana address. An address alone can be watched, for its balances. The
//! keys go to the system's vault (the Keychain, Credential Manager) as API
//! keys do, or a private file where there is none; `wallet.json` keeps only
//! the addresses, the networks and the rules. No tool ever returns a key,
//! and every key is among the secrets scrubbed from what tools return, so
//! one printed by a command is hidden too.
//!
//! The agent pays in two ways. A site that answers HTTP 402 is paid through
//! x402 (`x402.rs`): on its own when the user lets it, below a price they
//! set, otherwise after a yes on `paid_fetch`'s card. Money sent anywhere
//! else goes through `wallet_send`, whose card is shown in every mode, as a
//! workflow's is. Either way the caps in `policy.rs` hold even after a yes:
//! a cap is the most the user lets the agent spend, not a question to ask.
//! What the user sends by hand, from the terminal or the window, is theirs,
//! recorded but not counted.

mod chains;
mod evm;
mod keys;
mod policy;
mod solana;
mod x402;

pub use chains::{BASE, Chain, Family, Network, SOLANA, Token, custom_evm};
pub use policy::{
    DOLLAR, Entry, Kind, Policy, Spent, Status, X402, dollars, ledger, parse_dollars, spent,
};
pub(crate) use x402::on_402;

use crate::gate::Outcome;
use crate::tools::{Tool, ToolContext};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

/// The wallets, networks and rules, in the data folder.
pub const WALLET_FILE: &str = "wallet.json";
/// Keys, where the system has no vault, in the data folder.
const KEYS_FILE: &str = "wallet-keys.json";
/// What the vault names a wallet's key under, before its id.
const VAULT: &str = "wallet.";
/// How long a network or a paid page may take to answer.
const HTTP_TIME: Duration = Duration::from_secs(20);
/// Payments the `wallet` tool lists.
const HISTORY: usize = 20;

/// The tool that sends money.
pub const SEND: &str = "wallet_send";
/// The tool that pays for a page.
pub const PAID_FETCH: &str = "paid_fetch";

/// The wallets, the networks they use, and the rules for spending.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Wallet {
    pub accounts: Vec<Account>,
    /// The id of the wallet that pays when none is named.
    pub default: String,
    pub policy: Policy,
    /// What the user changed about each network, by CAIP-2 id.
    pub networks: BTreeMap<String, Network>,
    /// Networks the user added.
    pub chains: Vec<Chain>,
    /// Tokens the user added.
    pub tokens: Vec<Token>,
    /// Addresses the user saved, which the agent alone may send to when
    /// `policy.saved_only` is set.
    pub saved: Vec<Saved>,
}

/// One wallet: its addresses, and what it holds the key as.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Account {
    pub id: String,
    pub label: String,
    pub holds: Holds,
    /// Its EVM address, or empty when it has none.
    #[serde(default)]
    pub evm: String,
    /// Its Solana address, or empty when it has none.
    #[serde(default)]
    pub solana: String,
    /// The account number under its phrase: 0 for the first, as wallet
    /// apps number them.
    #[serde(default)]
    pub index: u32,
}

/// What a wallet's key is kept as.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Holds {
    /// A recovery phrase: both addresses.
    Phrase,
    /// A key taken from another app: one address.
    Key,
    /// Only an address, watched for its balances.
    Watch,
}

/// An address the user saved by name.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Saved {
    pub label: String,
    pub address: String,
}

/// What one wallet holds of one coin on one network.
#[derive(Clone, Debug, PartialEq)]
pub struct Holding {
    pub chain: String,
    pub symbol: String,
    /// As a person reads it.
    pub amount: String,
    pub usd: Option<u64>,
    /// Why it could not be read, when it could not.
    pub error: String,
}

impl Account {
    /// Its address on a network of `family`, or empty.
    pub fn address(&self, family: Family) -> &str {
        match family {
            Family::Evm => &self.evm,
            Family::Solana => &self.solana,
        }
    }
}

pub fn path() -> PathBuf {
    crate::chat::data_dir().join(WALLET_FILE)
}

fn keys_path() -> PathBuf {
    crate::chat::data_dir().join(KEYS_FILE)
}

/// The wallet as saved; empty when there is none.
pub fn load() -> Wallet {
    crate::state::read(&path()).unwrap_or_default()
}

/// Every network, with the user's changes, each saying whether it is on.
pub fn networks(wallet: &Wallet) -> Vec<Chain> {
    chains::all(wallet)
}

/// Where the wallets' keys are kept in this run, as a person reads it.
pub fn keys_kept() -> String {
    match crate::vault::label() {
        Some(vault) => format!("in {vault}"),
        None => format!("in {KEYS_FILE} in the data folder, which only you can read"),
    }
}

/// A send as it would go, checked as far as it can be without the network,
/// or why it cannot be made: for the question asked before it is signed.
pub fn describe_send(
    from: Option<&str>,
    network: &str,
    to: &str,
    amount: &str,
    token: &str,
) -> Result<String, String> {
    let (_, plan) = plan_send(from, network, to, amount, token, Kind::Own, false)?;
    Ok(format!(
        "Send {} {} on {} from {} to {}?",
        plan.entry.amount, plan.entry.token, plan.chain.name, plan.account.label, plan.entry.to
    ))
}

/// The network `name` means, by any of its names, with the user's changes.
pub fn network(wallet: &Wallet, name: &str) -> Option<Chain> {
    chains::find(&chains::all(wallet), name).cloned()
}

/// Change the wallet file under its lock, starting from what is on disk, and
/// give the wallet as saved; nothing is written when `edit` refuses.
pub fn change(edit: impl FnOnce(&mut Wallet) -> Result<(), String>) -> Result<Wallet, String> {
    std::fs::create_dir_all(crate::chat::data_dir()).map_err(|e| e.to_string())?;
    let mut outcome = Err("The wallet could not be changed.".to_string());
    crate::state::update::<Wallet>(&path(), |disk| {
        let mut wallet: Wallet = serde_json::from_value(disk.clone()).unwrap_or_default();
        outcome = edit(&mut wallet).and_then(|()| {
            *disk = serde_json::to_value(&wallet).map_err(|e| e.to_string())?;
            Ok(wallet)
        });
    })?;
    outcome
}

/// A new wallet with a new recovery phrase, and the phrase, which the user
/// must write down: it is the only way back to the money.
pub fn create(label: &str) -> Result<(Account, String), String> {
    let phrase = keys::new_phrase()?;
    let account = import(label, &phrase, 0)?;
    Ok((account, phrase))
}

/// A wallet from a recovery phrase (account `index` under it), an EVM key in
/// hex, or a Solana key, as another wallet app exports them.
pub fn import(label: &str, secret: &str, index: u32) -> Result<Account, String> {
    let (holds, evm, solana, kept) = match keys::read_secret(secret)? {
        keys::Secret::Phrase(phrase) => {
            let seed = keys::seed(&phrase)?;
            let evm = keys::evm_address(&keys::evm_from_seed(&seed, index)?)?;
            let solana = keys::solana_address(&keys::solana_from_seed(&seed, index))?;
            let kept = json!({"phrase": phrase, "index": index});
            (Holds::Phrase, evm, solana, kept)
        }
        keys::Secret::Evm(key) => {
            let kept = json!({"evm": format!("0x{}", keys::to_hex(&key))});
            (Holds::Key, keys::evm_address(&key)?, String::new(), kept)
        }
        keys::Secret::Solana(seed) => {
            let mut full = seed.to_vec();
            full.extend(keys::solana_public(&seed)?);
            let kept = json!({"solana": solana::to_base58(&full)});
            (
                Holds::Key,
                String::new(),
                keys::solana_address(&seed)?,
                kept,
            )
        }
    };
    let account = Account {
        id: new_id(),
        label: String::new(),
        holds,
        evm,
        solana,
        index,
    };
    add(label, account, Some(kept.to_string()))
}

/// A wallet that only watches `address`, EVM or Solana, for its balances.
pub fn watch(label: &str, address: &str) -> Result<Account, String> {
    let address = address.trim();
    let (evm, solana) = if address.starts_with("0x") {
        (keys::checksummed(&keys::evm_bytes(address)?), String::new())
    } else {
        solana::key(address)?;
        (String::new(), address.to_owned())
    };
    let account = Account {
        id: new_id(),
        label: String::new(),
        holds: Holds::Watch,
        evm,
        solana,
        index: 0,
    };
    add(label, account, None)
}

/// Keep `account` under `label` (or a numbered name), its key first; the
/// first wallet becomes the default.
fn add(label: &str, mut account: Account, secret: Option<String>) -> Result<Account, String> {
    let wallet = load();
    let same = |other: &Account| {
        (!account.evm.is_empty() && other.evm.eq_ignore_ascii_case(&account.evm))
            || (!account.solana.is_empty() && other.solana == account.solana)
    };
    if let Some(known) = wallet.accounts.iter().find(|other| same(other)) {
        return Err(format!("That wallet is here already, as {}.", known.label));
    }
    let label = label.trim();
    account.label = if label.is_empty() {
        format!("Wallet {}", wallet.accounts.len() + 1)
    } else {
        label.to_owned()
    };
    if find(&wallet, &account.label).is_some() {
        return Err(format!(
            "There is a wallet called {} already.",
            account.label
        ));
    }
    if let Some(secret) = secret {
        keep_secret(&account.id, &secret)?;
    }
    let kept = account.clone();
    change(move |wallet| {
        if wallet.default.is_empty() {
            wallet.default = account.id.clone();
        }
        wallet.accounts.push(account);
        Ok(())
    })?;
    Ok(kept)
}

/// The wallet `name` means: its name in any case, its id, or one of its
/// addresses.
pub fn find<'a>(wallet: &'a Wallet, name: &str) -> Option<&'a Account> {
    let name = name.trim();
    wallet.accounts.iter().find(|account| {
        account.label.eq_ignore_ascii_case(name)
            || account.id == name
            || (!account.evm.is_empty() && account.evm.eq_ignore_ascii_case(name))
            || (!account.solana.is_empty() && account.solana == name)
    })
}

/// The wallet `name` means, or why there is none.
fn named<'a>(wallet: &'a Wallet, name: &str) -> Result<&'a Account, String> {
    find(wallet, name).ok_or_else(|| format!("There is no wallet called {name}."))
}

pub fn rename(name: &str, label: &str) -> Result<(), String> {
    let label = label.trim().to_owned();
    if label.is_empty() {
        return Err("Give it a name.".into());
    }
    let name = name.to_owned();
    change(move |wallet| {
        if find(wallet, &label).is_some_and(|other| !other.label.eq_ignore_ascii_case(&name)) {
            return Err(format!("There is a wallet called {label} already."));
        }
        let id = named(wallet, &name)?.id.clone();
        if let Some(account) = wallet.accounts.iter_mut().find(|a| a.id == id) {
            account.label = label;
        }
        Ok(())
    })
    .map(|_| ())
}

/// Make `name` the wallet that pays when none is named.
pub fn make_default(name: &str) -> Result<(), String> {
    let name = name.to_owned();
    change(move |wallet| {
        wallet.default = named(wallet, &name)?.id.clone();
        Ok(())
    })
    .map(|_| ())
}

/// Take wallet `name` and its key out of Letronna; the money stays on the
/// chain, reachable with the phrase or key written down.
pub fn remove(name: &str) -> Result<(), String> {
    let wallet = load();
    let id = named(&wallet, name)?.id.clone();
    forget_secret(&id)?;
    change(move |wallet| {
        wallet.accounts.retain(|account| account.id != id);
        if wallet.default == id {
            wallet.default = wallet
                .accounts
                .first()
                .map(|a| a.id.clone())
                .unwrap_or_default();
        }
        Ok(())
    })
    .map(|_| ())
}

/// The recovery phrase or key of wallet `name`, for the user to write down
/// or move to another app; never for a tool.
pub fn reveal(name: &str) -> Result<String, String> {
    let wallet = load();
    let account = named(&wallet, name)?;
    let secret = secret_of(account)?;
    ["phrase", "evm", "solana"]
        .iter()
        .find_map(|field| secret[*field].as_str().map(str::to_owned))
        .ok_or_else(|| format!("{} keeps no key here.", account.label))
}

/// Switch a network on or off, or give it the user's own RPC address; an
/// empty address goes back to the public one.
pub fn set_network(name: &str, on: Option<bool>, rpc: Option<&str>) -> Result<Chain, String> {
    let wallet = load();
    let networks = chains::all(&wallet);
    let chain = chains::find(&networks, name)
        .ok_or_else(|| format!("There is no network called {name}."))?
        .clone();
    if let Some(rpc) = rpc.map(str::trim)
        && !rpc.is_empty()
        && !(rpc.starts_with("https://") || rpc.starts_with("http://"))
    {
        return Err(format!("`{rpc}` is not an RPC address (https://…)."));
    }
    let id = chain.id.clone();
    let rpc = rpc.map(|rpc| rpc.trim().to_owned());
    let wallet = change(move |wallet| {
        let set = wallet.networks.entry(id.clone()).or_default();
        if on.is_some() {
            set.on = on;
        }
        if let Some(rpc) = rpc {
            set.rpc = rpc;
        }
        if *set == Network::default() {
            wallet.networks.remove(&id);
        }
        Ok(())
    })?;
    chains::all(&wallet)
        .into_iter()
        .find(|c| c.id == chain.id)
        .ok_or_else(|| "The network went missing.".into())
}

/// Add an EVM network by its chain id and RPC address, after asking the RPC
/// which chain it serves, so a wrong id cannot sign for another chain.
pub fn add_network(
    name: &str,
    chain_id: u64,
    address: &str,
    symbol: &str,
) -> Result<Chain, String> {
    let chain = chains::custom_evm(name, chain_id, address, symbol)?;
    let answer = rpc(&chain, "eth_chainId", json!([]))?;
    let served = answer
        .as_str()
        .and_then(|id| u64::from_str_radix(id.trim_start_matches("0x"), 16).ok());
    if served != Some(chain_id) {
        return Err(format!(
            "That RPC serves chain {}, not {chain_id}.",
            served.map_or("?".into(), |id| id.to_string())
        ));
    }
    let added = chain.clone();
    change(move |wallet| {
        if chains::all(wallet).iter().any(|known| known.id == added.id) {
            return Err(format!("Chain {chain_id} is here already."));
        }
        wallet.chains.push(added);
        Ok(())
    })?;
    Ok(chain)
}

/// Add a token on `network` by its contract or mint address, its symbol and
/// decimals read from the chain; `dollar` when it is worth one US dollar.
pub fn add_token(network: &str, address: &str, dollar: bool) -> Result<Token, String> {
    let wallet = load();
    let networks = chains::all(&wallet);
    let chain = chains::find(&networks, network)
        .ok_or_else(|| format!("There is no network called {network}."))?;
    if chain.token(address).is_some() {
        return Err(format!("{} knows that token already.", chain.name));
    }
    let (decimals, symbol) = match chain.family {
        Family::Evm => evm::describe(chain, address)?,
        Family::Solana => (solana::decimals(chain, address)?, String::new()),
    };
    let symbol = if symbol.trim().is_empty() {
        address.chars().take(6).collect()
    } else {
        symbol
    };
    let token = Token {
        chain: chain.id.clone(),
        symbol,
        address: address.trim().to_owned(),
        decimals,
        dollar,
        signing_name: String::new(),
        signing_version: String::new(),
    };
    let added = token.clone();
    change(move |wallet| {
        wallet.tokens.push(added);
        Ok(())
    })?;
    Ok(token)
}

/// Save `address` under `label`, for sends and for `saved_only`.
pub fn save_address(label: &str, address: &str) -> Result<(), String> {
    let address = address.trim().to_owned();
    if keys::evm_bytes(&address).is_err() && solana::key(&address).is_err() {
        return Err(format!("`{address}` is not an EVM or Solana address."));
    }
    let label = label.trim().to_owned();
    change(move |wallet| {
        wallet.saved.retain(|saved| saved.address != address);
        wallet.saved.push(Saved { label, address });
        Ok(())
    })
    .map(|_| ())
}

/// The wallet that pays on a network of `family`: the default one when it
/// can, else the first that holds a key for it.
pub fn payer(wallet: &Wallet, family: Family) -> Option<&Account> {
    let can =
        |account: &&Account| account.holds != Holds::Watch && !account.address(family).is_empty();
    wallet
        .accounts
        .iter()
        .filter(can)
        .min_by_key(|account| account.id != wallet.default)
}

/// What `account` holds on each network that is on, read from each at once;
/// the native coin always, a token only when there is some.
pub fn balances(wallet: &Wallet, account: &Account) -> Vec<Holding> {
    let networks: Vec<Chain> = chains::all(wallet)
        .into_iter()
        .filter(|chain| chain.on && !account.address(chain.family).is_empty())
        .collect();
    std::thread::scope(|scope| {
        let reads: Vec<_> = networks
            .iter()
            .map(|chain| scope.spawn(move || held_on(chain, account)))
            .collect();
        reads
            .into_iter()
            .flat_map(|read| read.join().unwrap_or_default())
            .collect()
    })
}

/// What `account` holds on `chain`.
fn held_on(chain: &Chain, account: &Account) -> Vec<Holding> {
    let address = account.address(chain.family);
    let holding = |symbol: &str, decimals: u8, dollar: bool, read: Result<u128, String>| match read
    {
        Ok(atomic) => Holding {
            chain: chain.name.clone(),
            symbol: symbol.to_owned(),
            amount: amount_text(atomic, decimals),
            usd: if atomic > 0 {
                policy::worth(atomic, decimals, dollar, symbol)
            } else {
                Some(0)
            },
            error: String::new(),
        },
        Err(why) => Holding {
            chain: chain.name.clone(),
            symbol: symbol.to_owned(),
            amount: String::new(),
            usd: None,
            error: why,
        },
    };
    let native = match chain.family {
        Family::Evm => evm::balance(chain, address),
        Family::Solana => solana::balance(chain, address),
    };
    let mut held = vec![holding(&chain.symbol, chain.decimals, false, native)];
    for token in &chain.tokens {
        let read = match chain.family {
            Family::Evm => evm::token_balance(chain, token, address),
            Family::Solana => solana::token_balance(chain, address, token),
        };
        if read.as_ref().is_ok_and(|atomic| *atomic > 0) {
            held.push(holding(&token.symbol, token.decimals, token.dollar, read));
        }
    }
    held
}

/// Send `amount` of `token` (or the native coin) on `network` from wallet
/// `from` (or the one that pays) to `to`. The agent's sends meet the caps
/// and rules; the user's own are only recorded.
pub fn send(
    from: Option<&str>,
    network: &str,
    to: &str,
    amount: &str,
    token: &str,
    kind: Kind,
) -> Result<Entry, String> {
    let (wallet, plan) = plan_send(from, network, to, amount, token, kind, true)?;
    let Send {
        chain,
        account,
        token,
        atomic,
        mut entry,
    } = plan;
    policy::reserve(&wallet.policy, entry.clone())?;
    let sent = match chain.family {
        Family::Evm => {
            evm_key(&account).and_then(|key| evm::send(&chain, &key, to, atomic, token.as_ref()))
        }
        Family::Solana => solana_key(&account).and_then(|seed| {
            let atomic = u64::try_from(atomic).map_err(|_| "Too large an amount for Solana.")?;
            solana::send(&chain, &seed, to.trim(), atomic, token.as_ref())
        }),
    };
    match sent {
        Ok(tx) => {
            entry.link = chain.link(&tx);
            entry.tx = tx;
            entry.status = Status::Paid;
            policy::settle(&entry.id, Status::Paid, &entry.tx, &entry.link, "");
            Ok(entry)
        }
        Err(why) => {
            policy::settle(&entry.id, Status::Failed, "", "", &why);
            Err(why)
        }
    }
}

/// A send worked out before anything is signed.
struct Send {
    chain: Chain,
    account: Account,
    token: Option<Token>,
    atomic: u128,
    entry: Entry,
}

/// Work out a send, or why it cannot be made; a coin's dollar worth is
/// looked up only when `priced`, as a card is drawn without waiting on the
/// network.
fn plan_send(
    from: Option<&str>,
    network: &str,
    to: &str,
    amount: &str,
    token: &str,
    kind: Kind,
    priced: bool,
) -> Result<(Wallet, Send), String> {
    let wallet = load();
    let networks = chains::all(&wallet);
    let chain = chains::find(&networks, network)
        .ok_or_else(|| format!("There is no network called {network}."))?
        .clone();
    if !chain.on {
        return Err(format!("{} is switched off in the wallet.", chain.name));
    }
    let account = match from.filter(|from| !from.trim().is_empty()) {
        Some(name) => named(&wallet, name)?.clone(),
        None => payer(&wallet, chain.family)
            .ok_or_else(|| format!("No wallet here can send on {}.", chain.name))?
            .clone(),
    };
    if account.holds == Holds::Watch || account.address(chain.family).is_empty() {
        return Err(format!("{} cannot send on {}.", account.label, chain.name));
    }
    let to = to.trim();
    match chain.family {
        Family::Evm => keys::evm_bytes(to).map(|_| ())?,
        Family::Solana => solana::key(to).map(|_| ())?,
    }
    if kind != Kind::Own {
        if wallet.policy.paused {
            return Err("The user has paused the wallet's payments.".into());
        }
        let saved = wallet
            .saved
            .iter()
            .any(|s| s.address.eq_ignore_ascii_case(to));
        if wallet.policy.saved_only && !saved {
            return Err(format!(
                "{to} is not a saved address, and the user lets the agent send only to those."
            ));
        }
    }
    let token = if chain.is_native(token) {
        None
    } else {
        Some(chain.token(token).cloned().ok_or_else(|| {
            format!(
                "{} has no token {token} in the wallet; the user can add it with `letronna wallet tokens add`.",
                chain.name
            )
        })?)
    };
    let (symbol, decimals, dollar) = match &token {
        Some(token) => (token.symbol.clone(), token.decimals, token.dollar),
        None => (chain.symbol.clone(), chain.decimals, false),
    };
    let atomic = parse_amount(amount, decimals)?;
    if atomic == 0 {
        return Err("Say an amount above zero.".into());
    }
    let entry = Entry {
        id: new_id(),
        at: crate::chat::now() as i64,
        kind,
        status: Status::Pending,
        chain: chain.name.clone(),
        token: symbol.clone(),
        amount: amount_text(atomic, decimals),
        usd: (priced || dollar)
            .then(|| policy::worth(atomic, decimals, dollar, &symbol))
            .flatten(),
        to: to.to_owned(),
        site: String::new(),
        wallet: account.label.clone(),
        tx: String::new(),
        link: String::new(),
        note: String::new(),
    };
    Ok((
        wallet,
        Send {
            chain,
            account,
            token,
            atomic,
            entry,
        },
    ))
}

/// What the gate does with a call to a tool that pays: `None` for any
/// other tool. A send always goes to the user; a page's payment runs
/// unasked only when the user's x402 rules would pay it on their own, and
/// either is refused outright when the rules forbid it, so no card offers
/// what could not happen.
pub(crate) fn gate(name: &str, args: &Value, human: bool) -> Option<Outcome> {
    if name != SEND && name != PAID_FETCH {
        return None;
    }
    let wallet = load();
    let nobody = Outcome::Refuse(
        "Refused: the wallet pays only with the user's yes, and nobody is here to give it. Say what you would have paid for instead."
            .into(),
    );
    if wallet.policy.paused {
        return Some(Outcome::Refuse(
            "Refused: the user has paused the wallet's payments.".into(),
        ));
    }
    if name == SEND {
        return Some(if human { Outcome::Ask } else { nobody });
    }
    if wallet.policy.x402 == X402::Off {
        return Some(Outcome::Refuse(
            "Refused: the user has switched x402 payments off.".into(),
        ));
    }
    let url = args["url"].as_str().unwrap_or_default();
    let site = reqwest::Url::parse(url)
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_default();
    if let Err(why) = policy::site_allowed(&wallet.policy, &site) {
        return Some(Outcome::Refuse(format!("Refused: {why}")));
    }
    if let Some(usd) = x402::remembered(url).and_then(|seen| seen.usd) {
        let spent = spent(&ledger(), crate::chat::now() as i64);
        if let Err(why) = policy::room(&wallet.policy, spent, usd) {
            return Some(Outcome::Refuse(format!("Refused: {why}")));
        }
        if policy::automatic(&wallet.policy, Some(usd), &site) {
            return Some(Outcome::Run);
        }
    }
    Some(if human { Outcome::Ask } else { nobody })
}

/// The keys and phrases the wallets hold, in each form one could be shown
/// in, so none is ever let out in a tool's result or a card.
pub(crate) fn secrets() -> Vec<String> {
    let wallet = load();
    let mut all = Vec::new();
    for account in wallet.accounts.iter().filter(|a| a.holds != Holds::Watch) {
        let Ok(secret) = secret_of(account) else {
            continue;
        };
        for field in ["phrase", "evm", "solana"] {
            if let Some(text) = secret[field].as_str() {
                all.push(text.to_owned());
                if let Some(bare) = text.strip_prefix("0x") {
                    all.push(bare.to_owned());
                }
            }
        }
    }
    all
}

/// Put wallet `id`'s secret in the vault, or the keys file where there is
/// none, as `store::save_key` does for API keys.
fn keep_secret(id: &str, secret: &str) -> Result<(), String> {
    let name = format!("{VAULT}{id}");
    let mut refused = None;
    crate::state::update::<BTreeMap<String, String>>(&keys_path(), |disk| {
        let in_file = disk[id].as_str().unwrap_or_default().to_owned();
        match crate::vault::save(&name, secret, &in_file) {
            Ok(kept) if kept.is_empty() => {
                if let Some(keys) = disk.as_object_mut() {
                    keys.remove(id);
                }
            }
            Ok(kept) => match disk.as_object_mut() {
                Some(keys) => {
                    keys.insert(id.to_owned(), kept.into());
                }
                None => *disk = json!({ id: kept }),
            },
            Err(why) => refused = Some(why),
        }
    })?;
    refused.map_or(Ok(()), Err)
}

/// Wallet `account`'s secret: its phrase and index, or its key.
fn secret_of(account: &Account) -> Result<Value, String> {
    let file: BTreeMap<String, String> = crate::state::read(&keys_path()).unwrap_or_default();
    let text = match file.get(&account.id).filter(|text| !text.is_empty()) {
        Some(text) => text.clone(),
        None => crate::vault::get(&format!("{VAULT}{}", account.id))?
            .ok_or_else(|| format!("The key of {} is missing from the vault.", account.label))?,
    };
    serde_json::from_str(&text)
        .map_err(|_| format!("The key of {} could not be read.", account.label))
}

/// Take wallet `id`'s secret out of the vault and the keys file.
fn forget_secret(id: &str) -> Result<(), String> {
    crate::vault::remove(&format!("{VAULT}{id}"))?;
    crate::state::update::<BTreeMap<String, String>>(&keys_path(), |disk| {
        if let Some(keys) = disk.as_object_mut() {
            keys.remove(id);
        }
    })
}

/// The EVM key `account` signs with.
fn evm_key(account: &Account) -> Result<[u8; 32], String> {
    let secret = secret_of(account)?;
    if let Some(phrase) = secret["phrase"].as_str() {
        let index = secret["index"].as_u64().unwrap_or(0) as u32;
        return keys::evm_from_seed(&keys::seed(phrase)?, index);
    }
    let hex = secret["evm"]
        .as_str()
        .ok_or_else(|| format!("{} holds no EVM key.", account.label))?;
    keys::from_hex(hex)?
        .try_into()
        .map_err(|_| format!("The EVM key of {} is damaged.", account.label))
}

/// The Solana seed `account` signs with.
fn solana_key(account: &Account) -> Result<[u8; 32], String> {
    let secret = secret_of(account)?;
    if let Some(phrase) = secret["phrase"].as_str() {
        let index = secret["index"].as_u64().unwrap_or(0) as u32;
        return Ok(keys::solana_from_seed(&keys::seed(phrase)?, index));
    }
    let text = secret["solana"]
        .as_str()
        .ok_or_else(|| format!("{} holds no Solana key.", account.label))?;
    solana::from_base58(text)?
        .get(..32)
        .and_then(|seed| seed.try_into().ok())
        .ok_or_else(|| format!("The Solana key of {} is damaged.", account.label))
}

/// The wallets, rules and spending as a few lines, for the `wallet` tool
/// and the terminal.
pub fn summary(wallet: &Wallet) -> String {
    if wallet.accounts.is_empty() {
        return "There is no wallet yet. The user can make one with `letronna wallet new`, bring one with `letronna wallet import`, or use the window's Wallet page.".into();
    }
    let mut lines = vec!["Wallets:".to_owned()];
    for account in &wallet.accounts {
        let mut addresses = Vec::new();
        if !account.evm.is_empty() {
            addresses.push(format!("EVM {}", account.evm));
        }
        if !account.solana.is_empty() {
            addresses.push(format!("Solana {}", account.solana));
        }
        let mark = if account.id == wallet.default {
            " (pays by default)"
        } else {
            ""
        };
        let watch = if account.holds == Holds::Watch {
            " (watched only)"
        } else {
            ""
        };
        lines.push(format!(
            "- {}{mark}{watch}: {}",
            account.label,
            addresses.join(", ")
        ));
    }
    let policy = &wallet.policy;
    let spent = spent(&ledger(), crate::chat::now() as i64);
    let x402 = match policy.x402 {
        X402::Off => "never paid".to_owned(),
        X402::Ask => "each price asked about".to_owned(),
        X402::Auto => format!(
            "paid unasked up to {}, asked above",
            dollars(policy.auto_up_to)
        ),
    };
    lines.push(format!(
        "Sites that ask to be paid (x402): {x402}.{}",
        if policy.paused {
            " Payments are paused."
        } else {
            ""
        }
    ));
    lines.push(format!(
        "Caps: {} a payment, {} a day ({} spent), {} a month ({} spent).",
        dollars(policy.per_payment),
        dollars(policy.per_day),
        dollars(spent.day),
        dollars(policy.per_month),
        dollars(spent.month)
    ));
    let on: Vec<String> = chains::all(wallet)
        .into_iter()
        .filter(|chain| chain.on)
        .map(|chain| chain.name)
        .collect();
    lines.push(format!("Networks: {}.", on.join(", ")));
    lines.join("\n")
}

/// A payment as one line.
pub fn entry_line(entry: &Entry) -> String {
    let when = chrono::DateTime::from_timestamp(entry.at, 0)
        .map(|at| {
            at.with_timezone(&chrono::Local)
                .format("%b %-d %H:%M")
                .to_string()
        })
        .unwrap_or_default();
    let who = match entry.kind {
        Kind::X402 => format!(
            "paid {}",
            if entry.site.is_empty() {
                &entry.to
            } else {
                &entry.site
            }
        ),
        Kind::Send => format!("sent to {}", entry.to),
        Kind::Own => format!("you sent to {}", entry.to),
    };
    let usd = entry
        .usd
        .map(|usd| format!(" ({})", dollars(usd)))
        .unwrap_or_default();
    let status = match entry.status {
        Status::Paid => String::new(),
        Status::Pending => " · pending".into(),
        Status::Failed => format!(" · failed: {}", entry.note),
        Status::Unsettled => format!(" · not seen settled: {}", entry.note),
    };
    format!(
        "{when}  {} {}{usd} on {} from {}, {who}{status}",
        entry.amount, entry.token, entry.chain, entry.wallet
    )
}

/// One JSON-RPC call to `chain`. An RPC address can carry the user's key, so
/// no error says it.
pub(crate) fn rpc(chain: &Chain, method: &str, params: Value) -> Result<Value, String> {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params});
    let answer: Value = client()?
        .post(&chain.rpc)
        .json(&body)
        .send()
        .map_err(|e| format!("{} did not answer: {}", chain.name, e.without_url()))?
        .json()
        .map_err(|_| format!("{} answered something that is not JSON-RPC.", chain.name))?;
    if let Some(error) = answer.get("error").filter(|error| !error.is_null()) {
        let said = error["message"]
            .as_str()
            .map_or_else(|| error.to_string(), str::to_owned);
        return Err(format!("{} refused {method}: {said}", chain.name));
    }
    Ok(answer["result"].clone())
}

/// The HTTP client the wallet's calls go through. It follows no redirect:
/// a payment is made for the address the price was given for.
pub(crate) fn client() -> Result<&'static reqwest::blocking::Client, String> {
    static CLIENT: OnceLock<Result<reqwest::blocking::Client, String>> = OnceLock::new();
    CLIENT
        .get_or_init(|| {
            reqwest::blocking::Client::builder()
                .timeout(HTTP_TIME)
                .redirect(reqwest::redirect::Policy::none())
                .user_agent(concat!("Letronna/", env!("CARGO_PKG_VERSION")))
                .build()
                .map_err(|e| e.to_string())
        })
        .as_ref()
        .map_err(Clone::clone)
}

/// `N` bytes from the system's random source.
pub(crate) fn random<const N: usize>() -> Result<[u8; N], String> {
    let mut bytes = [0u8; N];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes)
        .map_err(|_| "The system gave no random bytes.".to_string())?;
    Ok(bytes)
}

/// A fresh id for a wallet or a payment.
fn new_id() -> String {
    random::<6>()
        .map(|bytes| keys::to_hex(&bytes))
        .unwrap_or_else(|_| crate::chat::now().to_string())
}

/// `atomic` of a coin with `decimals`, as a person writes it: `1.5`, `0.01`.
pub fn amount_text(atomic: u128, decimals: u8) -> String {
    let scale = 10u128.pow(decimals as u32);
    let whole = atomic / scale;
    let rest = atomic % scale;
    if rest == 0 {
        return whole.to_string();
    }
    let digits = format!("{rest:0width$}", width = decimals as usize);
    format!("{whole}.{}", digits.trim_end_matches('0'))
}

/// An amount a person wrote, such as `1.5`, in the smallest unit of a coin
/// with `decimals`.
pub fn parse_amount(text: &str, decimals: u8) -> Result<u128, String> {
    let text = text.trim();
    let bad = || format!("`{text}` is not an amount.");
    let (whole, rest) = text.split_once('.').unwrap_or((text, ""));
    let digits = |part: &str| part.chars().all(|c| c.is_ascii_digit());
    if (whole.is_empty() && rest.is_empty()) || !digits(whole) || !digits(rest) {
        return Err(bad());
    }
    if rest.len() > decimals as usize {
        return Err(format!(
            "`{text}` has more decimals than the coin's {decimals}."
        ));
    }
    let scale = 10u128.pow(decimals as u32);
    let whole: u128 = if whole.is_empty() {
        0
    } else {
        whole.parse().map_err(|_| bad())?
    };
    let rest: u128 = if rest.is_empty() {
        0
    } else {
        format!("{rest:0<width$}", width = decimals as usize)
            .parse()
            .map_err(|_| bad())?
    };
    whole
        .checked_mul(scale)
        .and_then(|n| n.checked_add(rest))
        .ok_or_else(bad)
}

/// A long address cut to its ends, as a card has room for.
fn short(address: &str) -> String {
    if address.chars().count() <= 14 {
        return address.to_owned();
    }
    let start: String = address.chars().take(6).collect();
    let end: String = address
        .chars()
        .rev()
        .take(4)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{start}…{end}")
}

/// What the agent has spent against the caps, as a card's last line.
fn spending_line(policy: &Policy) -> String {
    let spent = spent(&ledger(), crate::chat::now() as i64);
    format!(
        "Spent by the agent: {} of {} today, {} of {} this month.",
        dollars(spent.day),
        dollars(policy.per_day),
        dollars(spent.month),
        dollars(policy.per_month)
    )
}

/// The model's way to see the wallets, what they hold, and what was paid.
pub struct WalletInfo;

impl Tool for WalletInfo {
    fn name(&self) -> &'static str {
        "wallet"
    }
    fn description(&self) -> &'static str {
        "The user's crypto wallet in Letronna: its addresses (EVM chains and Solana), the user's spending rules and caps, what it holds on each network, and what was paid. It never shows a key. Pay a site that asks for payment (HTTP 402, x402) with paid_fetch, and send money with wallet_send."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "action": {"type": "string", "enum": ["summary", "balances", "history"], "description": "summary: wallets, rules and caps; balances: what each wallet holds, read from the networks; history: the latest payments."},
            "wallet": {"type": "string", "description": "For balances, one wallet by name; all when left out."}
        }, "required": ["action"]})
    }
    fn offered(&self, _: &crate::chat::Connection) -> bool {
        !load().accounts.is_empty()
    }
    fn concurrent(&self) -> bool {
        true
    }
    fn run(&self, args: &Value, _: &ToolContext) -> Result<String, String> {
        let wallet = load();
        match args["action"].as_str().unwrap_or("summary") {
            "balances" => {
                let wanted = args["wallet"]
                    .as_str()
                    .filter(|name| !name.trim().is_empty());
                let accounts: Vec<&Account> = match wanted {
                    Some(name) => vec![named(&wallet, name)?],
                    None => wallet.accounts.iter().collect(),
                };
                let mut lines = Vec::new();
                for account in accounts {
                    lines.push(format!("{}:", account.label));
                    for held in balances(&wallet, account) {
                        lines.push(match held.error.is_empty() {
                            true => format!(
                                "- {} {} on {}{}",
                                held.amount,
                                held.symbol,
                                held.chain,
                                held.usd
                                    .map(|usd| format!(" ({})", dollars(usd)))
                                    .unwrap_or_default()
                            ),
                            false => format!("- {} on {}: {}", held.symbol, held.chain, held.error),
                        });
                    }
                }
                Ok(lines.join("\n"))
            }
            "history" => {
                let entries = ledger();
                if entries.is_empty() {
                    return Ok("Nothing has been paid from the wallet yet.".into());
                }
                let lines: Vec<String> =
                    entries.iter().rev().take(HISTORY).map(entry_line).collect();
                Ok(lines.join("\n"))
            }
            _ => Ok(summary(&wallet)),
        }
    }
}

/// The model's way to send money, which the user reads on a card each time.
pub struct WalletSend;

impl Tool for WalletSend {
    fn name(&self) -> &'static str {
        SEND
    }
    fn description(&self) -> &'static str {
        "Send money from the user's wallet: an amount of a network's native coin or of a token (such as USDC) to an address. The user sees each send on a card and decides, and it must fit the caps they set. A payment to a site that asks for it (HTTP 402) goes through paid_fetch instead."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "network": {"type": "string", "description": "Base, Ethereum, Arbitrum, Optimism, Polygon, BNB Chain, Avalanche, Solana, a test network, or one the user added; a CAIP-2 id works too."},
            "to": {"type": "string", "description": "The address: 0x… on an EVM network, base58 on Solana."},
            "amount": {"type": "string", "description": "As a person writes it, such as \"5\" or \"0.25\"."},
            "token": {"type": "string", "description": "The token's symbol or address, such as USDC; the network's own coin when left out."},
            "from": {"type": "string", "description": "Which wallet pays, by name; the default one when left out."}
        }, "required": ["network", "to", "amount"]})
    }
    fn offered(&self, _: &crate::chat::Connection) -> bool {
        load()
            .accounts
            .iter()
            .any(|account| account.holds != Holds::Watch)
    }
    fn needs_approval(&self) -> bool {
        true
    }
    fn asked_each_time(&self) -> bool {
        true
    }
    fn mutates(&self) -> bool {
        true
    }
    fn destructive(&self) -> bool {
        true
    }
    fn open_world(&self) -> bool {
        true
    }
    fn preview(&self, args: &Value, _: &ToolContext) -> String {
        let text = |field: &str| args[field].as_str().unwrap_or_default().trim().to_owned();
        match plan_send(
            Some(&text("from")),
            &text("network"),
            &text("to"),
            &text("amount"),
            &text("token"),
            Kind::Send,
            false,
        ) {
            Ok((wallet, plan)) => {
                let usd = plan
                    .entry
                    .usd
                    .filter(|_| plan.token.as_ref().is_some_and(|t| t.dollar))
                    .map(|usd| format!(" ({})", dollars(usd)))
                    .unwrap_or_default();
                let fee = match plan.chain.family {
                    Family::Evm => format!(
                        "The network's fee, {} at most, is paid on top.",
                        dollars(evm::FEE_CEILING)
                    ),
                    Family::Solana => "The network's fee is paid on top.".to_owned(),
                };
                format!(
                    "Send {} {}{usd} on {}\nfrom {} ({})\nto {}\n{fee}\n{}",
                    plan.entry.amount,
                    plan.entry.token,
                    plan.chain.name,
                    plan.account.label,
                    short(plan.account.address(plan.chain.family)),
                    plan.entry.to,
                    spending_line(&wallet.policy)
                )
            }
            Err(why) => format!(
                "Send {} {} on {} to {}\n{why}",
                text("amount"),
                text("token"),
                text("network"),
                text("to")
            ),
        }
    }
    fn run(&self, args: &Value, ctx: &ToolContext) -> Result<String, String> {
        // A rule, a grant or a setting could let a call past the card;
        // money goes only with a yes to this very call.
        if !ctx.approved {
            return Err(
                "Nothing was sent: money goes only with the user's yes on this call's card.".into(),
            );
        }
        let text = |field: &str| args[field].as_str().unwrap_or_default().to_owned();
        let entry = send(
            Some(&text("from")),
            &text("network"),
            &text("to"),
            &text("amount"),
            &text("token"),
            Kind::Send,
        )?;
        let usd = entry
            .usd
            .map(|usd| format!(" ({})", dollars(usd)))
            .unwrap_or_default();
        Ok(format!(
            "Sent {} {}{usd} on {} from {} to {}. Transaction: {}{}",
            entry.amount,
            entry.token,
            entry.chain,
            entry.wallet,
            entry.to,
            entry.tx,
            if entry.link.is_empty() {
                String::new()
            } else {
                format!(" ({})", entry.link)
            }
        ))
    }
}

/// The model's way to read a page that asks to be paid (HTTP 402, x402).
pub struct PaidFetch;

impl Tool for PaidFetch {
    fn name(&self) -> &'static str {
        PAID_FETCH
    }
    fn description(&self) -> &'static str {
        "Read a page or call an API that asks to be paid (HTTP 402 with x402), paying it from the user's wallet in USDC or another token, then return what it gives. Use it when web_fetch reports a price, or for an API known to charge per call. The user's rules decide: a small price may be paid on its own, otherwise the user sees the price on a card; the caps always hold."
    }
    fn parameters(&self) -> Value {
        json!({"type": "object", "properties": {
            "url": {"type": "string", "description": "The full address, starting with https://."},
            "method": {"type": "string", "enum": ["GET", "POST"], "description": "GET when left out."},
            "body": {"type": "string", "description": "For POST, the JSON to send."},
            "max_usd": {"type": "string", "description": "The most to pay in dollars, such as \"0.05\"; the price shown on the card, or the per-payment cap, when left out."}
        }, "required": ["url"]})
    }
    fn offered(&self, _: &crate::chat::Connection) -> bool {
        load()
            .accounts
            .iter()
            .any(|account| account.holds != Holds::Watch)
    }
    fn needs_approval(&self) -> bool {
        true
    }
    fn asked_each_time(&self) -> bool {
        true
    }
    fn mutates(&self) -> bool {
        true
    }
    fn open_world(&self) -> bool {
        true
    }
    fn untrusted(&self) -> bool {
        true
    }
    fn hides_key_shapes(&self) -> bool {
        true
    }
    fn preview(&self, args: &Value, _: &ToolContext) -> String {
        let url = args["url"].as_str().unwrap_or_default();
        let method = args["method"].as_str().unwrap_or("GET");
        let wallet = load();
        let max = args["max_usd"]
            .as_str()
            .and_then(|max| parse_dollars(max).ok());
        let seen = x402::remembered(url);
        // The most this card lets the call pay, kept for `run`: the yes is
        // for what the card shows, whatever the page asks later.
        let most = max
            .into_iter()
            .chain(seen.as_ref().and_then(|seen| seen.usd))
            .min()
            .unwrap_or(wallet.policy.per_payment);
        x402::show(&args.to_string(), most);
        let price = match seen {
            Some(seen) if seen.usd.is_some_and(|usd| usd <= most) => {
                format!("Pay {} to {}", seen.price, seen.site)
            }
            Some(seen) => format!(
                "Pay up to {} ({} was asked by {})",
                dollars(most),
                seen.price,
                seen.site
            ),
            None => format!("Pay up to {} (the price is read as it runs)", dollars(most)),
        };
        format!(
            "{price}\nfor {method} {url}\n{}",
            spending_line(&wallet.policy)
        )
    }
    fn run(&self, args: &Value, ctx: &ToolContext) -> Result<String, String> {
        let raw = args["url"].as_str().unwrap_or_default().trim();
        let url = reqwest::Url::parse(raw).map_err(|_| format!("`{raw}` is not a web address."))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err("Only http and https addresses can be paid for.".into());
        }
        crate::browser::address(raw, &crate::redact::secrets())?;
        // Taken first, so a yes is spent even when the call fails early.
        let shown = ctx
            .approved
            .then(|| x402::shown(&args.to_string()))
            .flatten();
        let wallet = load();
        if wallet.policy.paused || wallet.policy.x402 == X402::Off {
            return Err("The user has the wallet's x402 payments switched off.".into());
        }
        let site = url.host_str().unwrap_or_default().to_owned();
        policy::site_allowed(&wallet.policy, &site)?;
        let method = match args["method"]
            .as_str()
            .map(str::to_ascii_uppercase)
            .as_deref()
        {
            Some("POST") => reqwest::Method::POST,
            _ => reqwest::Method::GET,
        };
        let body = args["body"]
            .as_str()
            .filter(|_| method == reqwest::Method::POST);
        let mut asking = client()?.request(method.clone(), url.clone());
        if let Some(body) = body {
            asking = asking
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.to_owned());
        }
        let first = asking
            .send()
            .map_err(|e| format!("Could not reach the page: {}", e.without_url()))?;
        if first.status().as_u16() != 402 {
            return readable(first, &url);
        }
        let headers = first.headers().clone();
        let said = first.bytes().unwrap_or_default();
        let quote = x402::read(&headers, &said)
            .ok_or("The page answered HTTP 402 but did not say how to pay it (no x402 offer).")?;
        let plan = x402::choose(&quote, &wallet, &site, true)?;
        x402::remember(url.as_str(), &plan);
        let price = plan.price();
        if ctx.approved {
            // Pay no more than the card showed, kept when it was drawn: the
            // remembered price may have lapsed or changed since.
            let most = shown.ok_or(
                "Nothing was paid: the card for this payment was not found. Call paid_fetch again.",
            )?;
            let usd = plan
                .usd
                .ok_or_else(|| format!("What {price} is worth in dollars is not known."))?;
            if usd > most {
                return Err(format!(
                    "Nothing was paid: it costs {price} now, more than the {} the user said yes to. Call paid_fetch again so they see the new price.",
                    dollars(most)
                ));
            }
        } else if !policy::automatic(&wallet.policy, plan.usd, &site) {
            return Err(format!(
                "Nothing was paid: it costs {price}, which needs the user's yes. Call paid_fetch again and the card shows the price."
            ));
        }
        let (page, entry) = x402::pay_and_fetch(&plan, method, &url, body, Kind::X402)?;
        let text = readable(page, &url)?;
        let receipt = match entry.tx.is_empty() {
            true => String::new(),
            false => format!(" Transaction: {}", entry.tx),
        };
        Ok(format!("Paid {price} to {site}.{receipt}\n\n{text}"))
    }
}

/// A page's answer as text for the model: HTML reduced as `web_fetch` does,
/// text and JSON as they are; an error status is the error.
fn readable(response: reqwest::blocking::Response, url: &reqwest::Url) -> Result<String, String> {
    use std::io::Read;
    let status = response.status();
    let kind = response
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_ascii_lowercase();
    let mut bytes = Vec::new();
    response
        .take(crate::web::MAX_BYTES)
        .read_to_end(&mut bytes)
        .map_err(|e| format!("The page stopped arriving: {e}"))?;
    let body = String::from_utf8_lossy(&bytes);
    let text = crate::web::readable(&body, &kind, url)
        .ok_or_else(|| format!("That is not a page of text ({kind})."))?;
    if !status.is_success() {
        return Err(format!(
            "The page answered HTTP {}.\n\n{text}",
            status.as_u16()
        ));
    }
    Ok(text)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::{DATA_DIR, Scratch};

    const ABANDON: &str = "abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon abandon about";

    /// Point the data folder at a scratch one for the length of a test.
    fn scratch(name: &str) -> (std::sync::MutexGuard<'static, ()>, Scratch) {
        let turn = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = Scratch::new(name);
        unsafe { std::env::set_var("AGENT_DATA_DIR", &*dir) };
        (turn, dir)
    }

    #[test]
    fn amounts_are_read_and_written_as_people_write_them() {
        assert_eq!(parse_amount("1.5", 6).unwrap(), 1_500_000);
        assert_eq!(parse_amount(".25", 6).unwrap(), 250_000);
        assert_eq!(parse_amount("3", 18).unwrap(), 3 * 10u128.pow(18));
        assert!(
            parse_amount("1.0000001", 6)
                .unwrap_err()
                .contains("decimals")
        );
        for bad in ["", ".", "-1", "1e5", "1,5", "abc"] {
            assert!(parse_amount(bad, 6).is_err(), "{bad}");
        }
        assert_eq!(amount_text(1_500_000, 6), "1.5");
        assert_eq!(amount_text(10_000, 6), "0.01");
        assert_eq!(amount_text(2 * 10u128.pow(18), 18), "2");
        assert_eq!(amount_text(1, 9), "0.000000001");
    }

    #[test]
    fn a_wallet_made_here_keeps_its_key_out_of_the_wallet_file() {
        let (_turn, dir) = scratch("wallet-create");
        let (made, phrase) = create("").unwrap();
        assert_eq!(made.label, "Wallet 1");
        assert!(made.evm.starts_with("0x") && !made.solana.is_empty());
        let file = std::fs::read_to_string(dir.join(WALLET_FILE)).unwrap();
        assert!(!file.contains(&phrase) && file.contains(&made.evm));
        assert_eq!(reveal("wallet 1").unwrap(), phrase);
        assert!(secrets().contains(&phrase));
        assert_eq!(load().default, made.id);
        // The same phrase twice is one wallet.
        assert!(import("Again", &phrase, 0).unwrap_err().contains("already"));
        // Its second account is another.
        let second = import("Second", &phrase, 1).unwrap();
        assert_ne!(second.evm, made.evm);
        remove("Wallet 1").unwrap();
        assert!(reveal("Wallet 1").is_err());
        let kept = std::fs::read_to_string(dir.join(KEYS_FILE)).unwrap();
        assert!(!kept.contains(&made.id) && kept.contains(&second.id));
        assert_eq!(load().default, second.id);
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_key_from_another_app_and_a_watched_address_are_each_one_address() {
        let (_turn, _dir) = scratch("wallet-import");
        let evm = import(
            "Hot",
            "0x4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318",
            0,
        )
        .unwrap();
        assert_eq!(evm.evm, "0x2c7536E3605D9C16a7a3D7b1898e529396a65c23");
        assert!(evm.solana.is_empty() && evm.holds == Holds::Key);
        assert!(
            secrets()
                .iter()
                .any(|s| s == "4c0883a69102937d6231471b5dbb6204fe5129617082792ae468d01a3f362318")
        );
        let watched = watch("Cold", "HAgk14JpMQLgt6rVgv7cBQFJWFto5Dqxi472uT3DKpqk").unwrap();
        assert!(watched.evm.is_empty() && watched.holds == Holds::Watch);
        assert!(reveal("cold").is_err());
        assert!(watch("Typo", "0x2c7536e3605D9C16a7a3D7b1898e529396a65c23").is_err());
        let wallet = load();
        assert_eq!(payer(&wallet, Family::Evm).unwrap().label, "Hot");
        assert!(payer(&wallet, Family::Solana).is_none());
        rename("hot", "Spending").unwrap();
        assert!(rename("Spending", "Cold").is_err());
        make_default("Cold").unwrap();
        assert_eq!(payer(&load(), Family::Evm).unwrap().label, "Spending");
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn the_agents_sends_meet_the_rules_before_anything_is_signed() {
        let (_turn, _dir) = scratch("wallet-send-rules");
        import("Main", ABANDON, 0).unwrap();
        let to = "0x209693Bc6afc0C5328bA36FaF03C514EF312287C";
        // Off, unknown, malformed and over-precise sends never reach a chain.
        let why = send(None, "base-sepolia", to, "1", "usdc", Kind::Send)
            .err()
            .unwrap();
        assert!(why.contains("switched off"), "{why}");
        assert!(
            send(None, "dogechain", to, "1", "", Kind::Send)
                .err()
                .unwrap()
                .contains("no network")
        );
        assert!(send(None, "base", "0x1234", "1", "usdc", Kind::Send).is_err());
        assert!(
            send(None, "base", to, "0.0000001", "usdc", Kind::Send)
                .err()
                .unwrap()
                .contains("decimals")
        );
        assert!(
            send(None, "base", to, "1", "pepe", Kind::Send)
                .err()
                .unwrap()
                .contains("tokens add")
        );
        assert_eq!(
            describe_send(None, "base", to, "0.5", "usdc").unwrap(),
            format!("Send 0.5 USDC on Base from Main to {to}?")
        );
        assert!(describe_send(None, "base", "0x1234", "1", "usdc").is_err());
        // Over the cap on one payment, refused before signing.
        let why = send(None, "base", to, "5", "usdc", Kind::Send)
            .err()
            .unwrap();
        assert!(why.contains("cap on one payment"), "{why}");
        change(|wallet| {
            wallet.policy.saved_only = true;
            Ok(())
        })
        .unwrap();
        assert!(
            send(None, "base", to, "0.5", "usdc", Kind::Send)
                .err()
                .unwrap()
                .contains("saved address")
        );
        change(|wallet| {
            wallet.policy.paused = true;
            Ok(())
        })
        .unwrap();
        assert!(
            send(None, "base", to, "0.5", "usdc", Kind::Send)
                .err()
                .unwrap()
                .contains("paused")
        );
        // Nothing reached the ledger.
        assert!(ledger().is_empty());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_send_is_asked_about_in_every_mode_and_refused_with_nobody_there() {
        let (_turn, _dir) = scratch("wallet-gate");
        import("Main", ABANDON, 0).unwrap();
        let args = json!({"network": "base", "to": "0x209693Bc6afc0C5328bA36FaF03C514EF312287C", "amount": "1"});
        assert_eq!(gate(SEND, &args, true), Some(Outcome::Ask));
        assert!(matches!(gate(SEND, &args, false), Some(Outcome::Refuse(_))));
        assert_eq!(gate("web_fetch", &args, true), None);
        // A page with no price seen yet is asked about; with x402 off, refused.
        let page = json!({"url": "https://api.paid.dev/data"});
        assert_eq!(gate(PAID_FETCH, &page, true), Some(Outcome::Ask));
        change(|wallet| {
            wallet.policy.x402 = X402::Off;
            Ok(())
        })
        .unwrap();
        assert!(matches!(
            gate(PAID_FETCH, &page, true),
            Some(Outcome::Refuse(_))
        ));
        change(|wallet| {
            wallet.policy.x402 = X402::Auto;
            wallet.policy.sites_block = vec!["paid.dev".into()];
            Ok(())
        })
        .unwrap();
        assert!(matches!(
            gate(PAID_FETCH, &page, true),
            Some(Outcome::Refuse(_))
        ));
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_page_whose_small_price_was_seen_runs_unasked_in_auto() {
        let (_turn, _dir) = scratch("wallet-gate-auto");
        import("Main", ABANDON, 0).unwrap();
        change(|wallet| {
            wallet.policy.x402 = X402::Auto;
            Ok(())
        })
        .unwrap();
        let wallet = load();
        let plan = |usd: u64| x402::Plan {
            quote: x402::Quote {
                version: 2,
                resource: Value::Null,
                accepts: Vec::new(),
                error: String::new(),
            },
            offer: Value::Null,
            chain: chains::all(&wallet).remove(0),
            token: chains::all(&wallet).remove(0).tokens.remove(0),
            amount: usd as u128,
            pay_to: String::new(),
            account: wallet.accounts[0].clone(),
            usd: Some(usd),
            site: "api.paid.dev".into(),
        };
        x402::remember("https://api.paid.dev/cheap", &plan(1_000));
        x402::remember("https://api.paid.dev/dear", &plan(DOLLAR / 2));
        x402::remember("https://api.paid.dev/over", &plan(2 * DOLLAR));
        let at = |url: &str| gate(PAID_FETCH, &json!({"url": url}), true);
        assert_eq!(at("https://api.paid.dev/cheap"), Some(Outcome::Run));
        assert_eq!(at("https://api.paid.dev/dear"), Some(Outcome::Ask));
        assert!(matches!(
            at("https://api.paid.dev/over"),
            Some(Outcome::Refuse(_))
        ));
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_users_network_must_be_the_chain_it_says() {
        let (_turn, _dir) = scratch("wallet-network");
        assert!(set_network("base", Some(false), None).is_ok_and(|chain| !chain.on));
        assert!(set_network("base", None, Some("ftp://x")).is_err());
        let chain = set_network("base", Some(true), Some("https://mine.example/rpc")).unwrap();
        assert!(chain.on && chain.rpc == "https://mine.example/rpc");
        let chain = set_network("base", None, Some("")).unwrap();
        assert_eq!(chain.rpc, "https://mainnet.base.org");
        assert_eq!(
            load().networks[BASE],
            Network {
                on: Some(true),
                rpc: String::new()
            }
        );
        // A network left at its defaults keeps no entry.
        set_network("solana", None, Some("")).unwrap();
        assert!(!load().networks.contains_key(SOLANA));
        assert!(set_network("dogechain", Some(true), None).is_err());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn the_summary_says_where_to_start_and_then_what_is_set() {
        let (_turn, _dir) = scratch("wallet-summary");
        assert!(summary(&load()).contains("letronna wallet new"));
        import("Main", ABANDON, 0).unwrap();
        let text = summary(&load());
        assert!(text.contains("Main (pays by default)"), "{text}");
        assert!(text.contains("0x9858EfFD232B4033E47d90003D41EC34EcaEda94"));
        assert!(text.contains("$1.00 a payment") && text.contains("each price asked about"));
        assert!(!text.contains("abandon"));
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }
}
