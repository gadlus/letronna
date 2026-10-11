//! `letronna wallet` and `/wallet`: the wallet the agent pays from, and the
//! rules it pays by.
//!
//! Each verb is written once, against `Keyboard`: outside a session that is
//! the terminal's own prompts, inside one the box, so a phrase typed in
//! either is drawn as dots and kept out of the scrollback. What a verb has
//! to say comes back as text for the caller to print where it prints.

use crate::render;
use letronna_core::wallet::{self, Kind, X402};
use std::io::IsTerminal;

const HELP: &str = "\
Your wallet: pay sites that ask to be paid (x402) and send money, within limits you set

  letronna wallet                       wallets, rules and what was spent
  letronna wallet new [name]            make a wallet with a new recovery phrase
  letronna wallet import [name]         bring one in by its recovery phrase or a key
  letronna wallet watch <address> [name]
                                        follow an address's balances only
  letronna wallet list                  the wallets and their addresses
  letronna wallet balance [name]        what each holds on every network
  letronna wallet history               the latest payments
  letronna wallet send <amount> [token] to <address> on <network>
                                        send money yourself; asks first
  letronna wallet limits                the caps; change them with the options below
  letronna wallet x402 off|ask|auto     what the agent does when a site asks to be paid
  letronna wallet pause                 stop the agent paying; resume lets it again
  letronna wallet sites allow|block|clear [site]
                                        pay only these sites, or never these
  letronna wallet chains                the networks; on|off <network>,
                                        rpc <network> <url|default>,
                                        add <name> <chain-id> <rpc> [symbol],
                                        testnets on|off
  letronna wallet prefer <network>...   the networks tried first when a site takes several
  letronna wallet tokens add <network> <address>
                                        a token to show and send
  letronna wallet save <name> <address> keep an address to send to
  letronna wallet only-saved on|off     the agent sends only to saved addresses
  letronna wallet default <name>        the wallet that pays when none is named
  letronna wallet rename <name> <new>   call a wallet something else
  letronna wallet reveal <name>         show its recovery phrase or key
  letronna wallet remove <name>         take a wallet out of Letronna

Options
      --index <n>          import: which account under the phrase, from 0
      --from <name>        send: the wallet that pays
      --chain <network>    balance: one network only
      --auto-up-to <$>     limits: the most an x402 payment costs to be paid unasked
      --per-payment <$>    limits: the cap on one payment by the agent
      --per-day <$>        limits: the cap on what it spends in 24 hours
      --per-month <$>      limits: the cap on what it spends in 30 days
      --dollar             tokens add: the token is worth one US dollar

Keys stay in the system's vault; the agent never sees them. The caps hold
even when you say yes to a payment; what you send yourself is not counted.
A command that sends, shows a key or changes the rules runs only when typed
at a terminal.";

/// What a verb needs from whoever is at the keyboard.
pub trait Keyboard {
    /// A line drawn as dots: a phrase or a key.
    fn secret(&mut self, hint: &str) -> Option<String>;
    /// A plain line.
    fn line(&mut self, hint: &str) -> Option<String>;
    /// One of `options`, or none when turned down.
    fn choose(&mut self, question: &str, detail: Vec<String>, options: &[&str]) -> Option<usize>;
    /// `work`, with something moving on the screen while the network answers.
    fn busy(&mut self, label: &str, work: Work) -> Result<String, String>;
}

/// Work done off the screen's thread.
pub type Work = Box<dyn FnOnce() -> Result<String, String> + Send>;

/// The terminal's own prompts, outside a session.
struct Plain;

impl Keyboard for Plain {
    fn secret(&mut self, hint: &str) -> Option<String> {
        crate::input::secret(&format!("{hint}: "))
    }
    fn line(&mut self, hint: &str) -> Option<String> {
        crate::input::read(&format!("{hint}: "))
    }
    fn choose(&mut self, question: &str, detail: Vec<String>, options: &[&str]) -> Option<usize> {
        println!("{question}");
        for line in detail {
            println!("{}", render::dim(&line));
        }
        crate::input::menu(options, 0)
    }
    fn busy(&mut self, label: &str, work: Work) -> Result<String, String> {
        crate::input::busy(label, work)
    }
}

impl Keyboard for crate::bar::Bar {
    fn secret(&mut self, hint: &str) -> Option<String> {
        self.ask(hint, true)
    }
    fn line(&mut self, hint: &str) -> Option<String> {
        self.ask(hint, false)
    }
    fn choose(&mut self, question: &str, detail: Vec<String>, options: &[&str]) -> Option<usize> {
        crate::bar::Bar::choose(self, question, detail, options)
    }
    fn busy(&mut self, label: &str, work: Work) -> Result<String, String> {
        crate::bar::Bar::busy(self, label, work)
    }
}

/// Run `letronna wallet <words>` and give the exit code.
pub fn run(words: &[String]) -> i32 {
    if matches!(
        words.first().map(String::as_str),
        Some("-h" | "--help" | "help")
    ) {
        println!("{HELP}");
        return 0;
    }
    // The agent's own shell can run this, with answers piped into the
    // prompts; so can a script a page talked it into. Only someone at a
    // terminal sends, shows a key or loosens the rules.
    if !std::io::stdin().is_terminal() && !reads_only(words) {
        eprintln!(
            "{}",
            render::red(
                "This wallet command sends money, shows a key or changes the wallet's rules, so it runs only when typed at a terminal. Nothing was done."
            )
        );
        return 1;
    }
    match verb(words, &mut Plain) {
        Ok(text) => {
            println!("{text}");
            0
        }
        Err(why) => {
            eprintln!("{}", render::red(&why));
            1
        }
    }
}

/// Do what `words` ask of the wallet, and say what came of it.
pub fn verb(words: &[String], keys: &mut dyn Keyboard) -> Result<String, String> {
    let (words, flags) = split_flags(words)?;
    let word = |at: usize| words.get(at).map(String::as_str).unwrap_or_default();
    let rest = |from: usize| words.get(from..).unwrap_or_default().join(" ");
    match word(0) {
        "" | "summary" => Ok(wallet::summary(&wallet::load())),
        "help" => Ok(HELP.into()),
        "new" => {
            let (made, phrase) = wallet::create(&rest(1))?;
            Ok(format!(
                "Made {}.\n{}\n\nIts recovery phrase -- write it down and keep it offline; it is the only way back to this money:\n\n  {}\n\nLetronna keeps it {}. `letronna wallet reveal {}` shows it again.",
                render::bold(&made.label),
                addresses(&made),
                render::bold(&phrase),
                wallet::keys_kept(),
                made.label
            ))
        }
        "import" => {
            let index = match flags.get("--index") {
                Some(n) => n
                    .parse()
                    .map_err(|_| format!("`{n}` is not an account number."))?,
                None => 0,
            };
            let secret = keys
                .secret("Recovery phrase, or a private key from another wallet app")
                .filter(|secret| !secret.trim().is_empty())
                .ok_or("Nothing was brought in.")?;
            let made = wallet::import(&rest(1), &secret, index)?;
            Ok(format!(
                "Brought in {}.\n{}",
                render::bold(&made.label),
                addresses(&made)
            ))
        }
        "watch" => {
            let address = words
                .get(1)
                .ok_or("Say which address: letronna wallet watch <address>")?;
            let made = wallet::watch(&rest(2), address)?;
            Ok(format!(
                "Watching {} (balances only).\n{}",
                render::bold(&made.label),
                addresses(&made)
            ))
        }
        "list" => list(),
        "balance" | "balances" => balance(keys, &rest(1), flags.get("--chain").cloned()),
        "history" => {
            let entries = wallet::ledger();
            if entries.is_empty() {
                return Ok("Nothing has been paid from the wallet yet.".into());
            }
            Ok(entries
                .iter()
                .rev()
                .take(30)
                .map(wallet::entry_line)
                .collect::<Vec<_>>()
                .join("\n"))
        }
        "send" => send(keys, &words[1..], flags.get("--from").map(String::as_str)),
        "limits" => limits(&flags),
        "x402" => x402(keys, word(1)),
        "pause" | "resume" => {
            let paused = word(0) == "pause";
            wallet::change(|w| {
                w.policy.paused = paused;
                Ok(())
            })?;
            Ok(if paused {
                "The agent pays nothing and sends nothing until `letronna wallet resume`.".into()
            } else {
                "The agent may pay again, within the rules.".into()
            })
        }
        "sites" => sites(word(1), word(2)),
        "chains" | "networks" => chains(&words[1..]),
        "prefer" => {
            let kept = wallet::load();
            if words.len() == 1 {
                let names: Vec<String> = kept
                    .policy
                    .prefer
                    .iter()
                    .filter_map(|id| wallet::network(&kept, id).map(|chain| chain.name))
                    .collect();
                return Ok(format!(
                    "Tried first: {}.",
                    if names.is_empty() {
                        "none".into()
                    } else {
                        names.join(", ")
                    }
                ));
            }
            let order = words[1..]
                .iter()
                .map(|name| {
                    wallet::network(&kept, name)
                        .map(|chain| chain.id)
                        .ok_or_else(|| format!("There is no network called {name}."))
                })
                .collect::<Result<Vec<String>, String>>()?;
            wallet::change(move |w| {
                w.policy.prefer = order;
                Ok(())
            })?;
            Ok("When a site takes several networks, those are tried first, in that order.".into())
        }
        "tokens" => match (word(1), word(2), word(3)) {
            ("add", network, address) if !network.is_empty() && !address.is_empty() => {
                let token = wallet::add_token(network, address, flags.contains_key("--dollar"))?;
                Ok(format!(
                    "Added {} ({} decimals) on {network}.",
                    token.symbol, token.decimals
                ))
            }
            _ => Err("Say the network and the address: letronna wallet tokens add base 0x…".into()),
        },
        "save" => match &words[1..] {
            [name, address] => {
                wallet::save_address(name, address)?;
                Ok(format!("Saved {address} as {name}."))
            }
            _ => Err("Say a name and an address: letronna wallet save shop 0x…".into()),
        },
        "only-saved" => {
            let on = on_off(word(1))?;
            wallet::change(|w| {
                w.policy.saved_only = on;
                Ok(())
            })?;
            Ok(if on {
                "The agent sends only to saved addresses.".into()
            } else {
                "The agent may send to any address, each one asked about.".into()
            })
        }
        "default" => {
            let name = rest(1);
            wallet::make_default(&name)?;
            Ok(format!("{name} pays when no wallet is named."))
        }
        "rename" => match &words[1..] {
            [name, new @ ..] if !new.is_empty() => {
                wallet::rename(name, &new.join(" "))?;
                Ok(format!("{name} is now {}.", new.join(" ")))
            }
            _ => Err(
                "Say which wallet and its new name: letronna wallet rename Main Spending".into(),
            ),
        },
        "reveal" => {
            let name = named(&rest(1))?;
            let typed = keys.line(&format!("Type {name} to show its recovery phrase or key"));
            if !typed.is_some_and(|typed| typed.trim().eq_ignore_ascii_case(&name)) {
                return Err("Nothing was shown.".into());
            }
            let secret = wallet::reveal(&name)?;
            Ok(format!(
                "{}\n\nAnyone with this can take the money. Keep it offline.",
                render::bold(&secret)
            ))
        }
        "remove" => {
            let name = named(&rest(1))?;
            let typed = keys.line(&format!(
                "Its money stays on the chain, reachable only with its phrase or key written down. Type {name} to remove it"
            ));
            if !typed.is_some_and(|typed| typed.trim().eq_ignore_ascii_case(&name)) {
                return Err("Nothing was removed.".into());
            }
            wallet::remove(&name)?;
            Ok(format!("Removed {name} from Letronna."))
        }
        other => Err(format!(
            "No wallet command `{other}`. See letronna wallet --help."
        )),
    }
}

/// Whether `words` only read the wallet: what it holds, its rules and what
/// was spent, with nothing asked. Any other verb, a new one included, waits
/// for a terminal.
fn reads_only(words: &[String]) -> bool {
    let Ok((words, flags)) = split_flags(words) else {
        // Refused by `verb` before it does anything.
        return true;
    };
    let words: Vec<&str> = words.iter().map(String::as_str).collect();
    match words.as_slice() {
        ["balance" | "balances", ..] => true,
        []
        | ["summary" | "help" | "list" | "history" | "sites" | "chains" | "networks" | "prefer"] => {
            true
        }
        ["limits"] => flags.is_empty(),
        _ => false,
    }
}

/// The words without their `--flags`, and each flag's value (empty for one
/// that takes none).
fn split_flags(
    words: &[String],
) -> Result<(Vec<String>, std::collections::HashMap<String, String>), String> {
    const VALUED: &[&str] = &[
        "--index",
        "--from",
        "--chain",
        "--auto-up-to",
        "--per-payment",
        "--per-day",
        "--per-month",
    ];
    const BARE: &[&str] = &["--dollar"];
    let mut plain = Vec::new();
    let mut flags = std::collections::HashMap::new();
    let mut words = words.iter();
    while let Some(word) = words.next() {
        if VALUED.contains(&word.as_str()) {
            let value = words
                .next()
                .ok_or_else(|| format!("{word} needs a value."))?;
            flags.insert(word.clone(), value.clone());
        } else if BARE.contains(&word.as_str()) {
            flags.insert(word.clone(), String::new());
        } else if word.starts_with("--") {
            return Err(format!(
                "Unknown option {word}. See letronna wallet --help."
            ));
        } else {
            plain.push(word.clone());
        }
    }
    Ok((plain, flags))
}

/// A wallet's addresses, a line each.
fn addresses(account: &wallet::Account) -> String {
    let mut lines = Vec::new();
    if !account.evm.is_empty() {
        lines.push(format!(
            "  EVM     {}  (Base, Ethereum and every EVM chain)",
            account.evm
        ));
    }
    if !account.solana.is_empty() {
        lines.push(format!("  Solana  {}", account.solana));
    }
    lines.join("\n")
}

/// The wallet's name as it is kept, from what was typed.
fn named(name: &str) -> Result<String, String> {
    if name.trim().is_empty() {
        return Err("Say which wallet.".into());
    }
    let kept = wallet::load();
    wallet::find(&kept, name)
        .map(|account| account.label.clone())
        .ok_or_else(|| format!("There is no wallet called {name}."))
}

fn list() -> Result<String, String> {
    let kept = wallet::load();
    if kept.accounts.is_empty() {
        return Ok(wallet::summary(&kept));
    }
    let mut lines = Vec::new();
    for account in &kept.accounts {
        let mark = if account.id == kept.default {
            "  (pays by default)"
        } else {
            ""
        };
        let kind = match account.holds {
            wallet::Holds::Phrase => "recovery phrase",
            wallet::Holds::Key => "key",
            wallet::Holds::Watch => "watched only",
        };
        lines.push(format!(
            "{}  {}{mark}",
            render::bold(&account.label),
            render::dim(kind)
        ));
        lines.push(addresses(account));
    }
    if !kept.saved.is_empty() {
        lines.push(String::new());
        lines.push(render::bold("Saved addresses"));
        for saved in &kept.saved {
            lines.push(format!("  {}  {}", saved.label, saved.address));
        }
    }
    Ok(lines.join("\n"))
}

fn balance(keys: &mut dyn Keyboard, name: &str, chain: Option<String>) -> Result<String, String> {
    let kept = wallet::load();
    let accounts: Vec<wallet::Account> = if name.trim().is_empty() {
        kept.accounts.clone()
    } else {
        vec![
            wallet::find(&kept, name)
                .ok_or_else(|| format!("There is no wallet called {name}."))?
                .clone(),
        ]
    };
    if accounts.is_empty() {
        return Ok(wallet::summary(&kept));
    }
    keys.busy(
        "reading balances",
        Box::new(move || {
            let mut lines = Vec::new();
            for account in &accounts {
                lines.push(render::bold(&account.label));
                let held = wallet::balances(&kept, account);
                let only = chain
                    .as_deref()
                    .map(|c| wallet::network(&kept, c).map(|chain| chain.name));
                let held = held.iter().filter(|h| {
                    only.as_ref()
                        .is_none_or(|name| name.as_deref() == Some(h.chain.as_str()))
                });
                let mut any = false;
                for h in held {
                    any = true;
                    lines.push(match h.error.is_empty() {
                        true => format!(
                            "  {:<16} {} {}{}",
                            h.chain,
                            h.amount,
                            h.symbol,
                            h.usd
                                .filter(|usd| *usd > 0)
                                .map(|usd| render::dim(&format!("  {}", wallet::dollars(usd))))
                                .unwrap_or_default()
                        ),
                        false => format!("  {:<16} {}", h.chain, render::dim(&h.error)),
                    });
                }
                if !any {
                    lines.push(render::dim("  no network matches"));
                }
            }
            Ok(lines.join("\n"))
        }),
    )
}

/// `send <amount> [token] to <address> on <network>`, asked about first.
fn send(keys: &mut dyn Keyboard, words: &[String], from: Option<&str>) -> Result<String, String> {
    let usage = "Say it as: letronna wallet send 5 usdc to 0x… on base";
    let to_at = words.iter().position(|w| w == "to").ok_or(usage)?;
    let on_at = words.iter().position(|w| w == "on").ok_or(usage)?;
    let (amount, token) = match &words[..to_at] {
        [amount] => (amount.clone(), String::new()),
        [amount, token] => (amount.clone(), token.clone()),
        _ => return Err(usage.into()),
    };
    let to = words
        .get(to_at + 1)
        .filter(|_| on_at == to_at + 2)
        .ok_or(usage)?
        .clone();
    let network = words.get(on_at + 1..).unwrap_or_default().join(" ");
    if network.is_empty() {
        return Err(usage.into());
    }
    let question = wallet::describe_send(from, &network, &to, &amount, &token)?;
    let detail =
        vec!["A transfer cannot be taken back. The network's fee is paid on top.".to_owned()];
    if keys.choose(&question, detail, &["Send", "Don't send"]) != Some(0) {
        return Err("Nothing was sent.".into());
    }
    let from = from.map(str::to_owned);
    keys.busy(
        "sending",
        Box::new(move || {
            let entry = wallet::send(from.as_deref(), &network, &to, &amount, &token, Kind::Own)?;
            let usd = entry
                .usd
                .map(|usd| format!(" ({})", wallet::dollars(usd)))
                .unwrap_or_default();
            Ok(format!(
                "Sent {} {}{usd} on {} from {} to {}.\nTransaction: {}{}",
                entry.amount,
                entry.token,
                entry.chain,
                entry.wallet,
                entry.to,
                entry.tx,
                if entry.link.is_empty() {
                    String::new()
                } else {
                    format!("\n{}", entry.link)
                }
            ))
        }),
    )
}

fn limits(flags: &std::collections::HashMap<String, String>) -> Result<String, String> {
    let read = |flag: &str| {
        flags
            .get(flag)
            .map(|value| wallet::parse_dollars(value))
            .transpose()
    };
    let (auto, payment, day, month) = (
        read("--auto-up-to")?,
        read("--per-payment")?,
        read("--per-day")?,
        read("--per-month")?,
    );
    let kept = if [auto, payment, day, month].iter().any(Option::is_some) {
        wallet::change(|w| {
            let policy = &mut w.policy;
            policy.auto_up_to = auto.unwrap_or(policy.auto_up_to);
            policy.per_payment = payment.unwrap_or(policy.per_payment);
            policy.per_day = day.unwrap_or(policy.per_day);
            policy.per_month = month.unwrap_or(policy.per_month);
            if policy.auto_up_to > policy.per_payment {
                return Err(
                    "What is paid unasked cannot be more than the cap on one payment.".into(),
                );
            }
            Ok(())
        })?
    } else {
        wallet::load()
    };
    let policy = &kept.policy;
    let spent = wallet::spent(&wallet::ledger(), chrono::Utc::now().timestamp());
    Ok(format!(
        "One payment by the agent: at most {}\nIn 24 hours: at most {} ({} spent)\nIn 30 days: at most {} ({} spent)\nPaid unasked when x402 is on auto: up to {}",
        wallet::dollars(policy.per_payment),
        wallet::dollars(policy.per_day),
        wallet::dollars(spent.day),
        wallet::dollars(policy.per_month),
        wallet::dollars(spent.month),
        wallet::dollars(policy.auto_up_to)
    ))
}

fn x402(keys: &mut dyn Keyboard, word: &str) -> Result<String, String> {
    let choices = [
        (X402::Off, "Off: never pay a site"),
        (X402::Ask, "Ask: show me each price"),
        (X402::Auto, "Auto: pay small prices on my behalf, ask above"),
    ];
    let picked = match word {
        "off" => X402::Off,
        "ask" => X402::Ask,
        "auto" => X402::Auto,
        "" => {
            let now = wallet::load().policy.x402;
            let labels: Vec<&str> = choices.iter().map(|(_, label)| *label).collect();
            let current = choices
                .iter()
                .find(|(mode, _)| *mode == now)
                .map(|(_, l)| *l)
                .unwrap_or_default();
            let detail = vec![format!("Now: {current}")];
            let Some(at) = keys.choose("When a site asks to be paid (x402)", detail, &labels)
            else {
                return Ok(format!("Left as it was. {current}."));
            };
            choices[at].0
        }
        other => return Err(format!("Say off, ask or auto, not {other}.")),
    };
    let kept = wallet::change(|w| {
        w.policy.x402 = picked;
        Ok(())
    })?;
    Ok(match picked {
        X402::Off => "Sites that ask to be paid are never paid.".into(),
        X402::Ask => "Each price is shown to you first.".into(),
        X402::Auto => format!(
            "Prices up to {} are paid on their own, within the caps; above that you are asked.",
            wallet::dollars(kept.policy.auto_up_to)
        ),
    })
}

fn sites(action: &str, site: &str) -> Result<String, String> {
    let site = site
        .trim()
        .trim_start_matches("https://")
        .trim_end_matches('/')
        .to_lowercase();
    let kept = match action {
        "allow" | "block" if site.is_empty() => {
            return Err(format!(
                "Say which site: letronna wallet sites {action} api.example.com"
            ));
        }
        "allow" => wallet::change(|w| {
            w.policy.sites_block.retain(|s| *s != site);
            if !w.policy.sites_allow.contains(&site) {
                w.policy.sites_allow.push(site.clone());
            }
            Ok(())
        })?,
        "block" => wallet::change(|w| {
            w.policy.sites_allow.retain(|s| *s != site);
            if !w.policy.sites_block.contains(&site) {
                w.policy.sites_block.push(site.clone());
            }
            Ok(())
        })?,
        "clear" => wallet::change(|w| {
            if site.is_empty() {
                w.policy.sites_allow.clear();
                w.policy.sites_block.clear();
            } else {
                w.policy.sites_allow.retain(|s| *s != site);
                w.policy.sites_block.retain(|s| *s != site);
            }
            Ok(())
        })?,
        "" => wallet::load(),
        other => return Err(format!("Say allow, block or clear, not {other}.")),
    };
    let policy = &kept.policy;
    let allowed = if policy.sites_allow.is_empty() {
        "any site".to_owned()
    } else {
        format!("only {}", policy.sites_allow.join(", "))
    };
    let blocked = if policy.sites_block.is_empty() {
        "none".to_owned()
    } else {
        policy.sites_block.join(", ")
    };
    Ok(format!("Paid: {allowed}\nNever paid: {blocked}"))
}

fn chains(words: &[String]) -> Result<String, String> {
    let word = |at: usize| words.get(at).map(String::as_str).unwrap_or_default();
    match word(0) {
        "" => {
            let kept = wallet::load();
            let mut lines = Vec::new();
            for chain in wallet::networks(&kept) {
                let state = if chain.on { render::green("on ") } else { render::dim("off") };
                let test = if chain.testnet { render::dim("  test network") } else { String::new() };
                let own = kept.networks.get(&chain.id).is_some_and(|n| !n.rpc.is_empty());
                let rpc = if own { render::dim("  your RPC") } else { String::new() };
                lines.push(format!("{state}  {:<16} {}{test}{rpc}", chain.name, render::dim(&chain.id)));
            }
            lines.push(render::dim("letronna wallet chains on|off <network> · rpc <network> <url|default> · add <name> <chain-id> <rpc> [symbol] · testnets on|off"));
            Ok(lines.join("\n"))
        }
        "on" | "off" => {
            let name = words[1..].join(" ");
            let chain = wallet::set_network(&name, Some(word(0) == "on"), None)?;
            Ok(format!("{} is {}.", chain.name, if chain.on { "on" } else { "off" }))
        }
        "rpc" => {
            let (name, url) = match &words[1..] {
                [name @ .., url] if !name.is_empty() => (name.join(" "), url.clone()),
                _ => return Err("Say the network and the address: letronna wallet chains rpc base https://…".into()),
            };
            let url = if url == "default" { String::new() } else { url };
            let chain = wallet::set_network(&name, None, Some(&url))?;
            Ok(format!("{} is read through {}.", chain.name, chain.rpc))
        }
        "add" => match &words[1..] {
            [name, id, rpc, symbol @ ..] => {
                let id: u64 = id.parse().map_err(|_| format!("`{id}` is not a chain id."))?;
                let chain = wallet::add_network(name, id, rpc, symbol.first().map(String::as_str).unwrap_or_default())?;
                Ok(format!("Added {} ({}), paying fees in {}.", chain.name, chain.id, chain.symbol))
            }
            _ => Err("Say a name, its chain id and an RPC address: letronna wallet chains add Sei 1329 https://evm-rpc.sei-apis.com SEI".into()),
        },
        "testnets" => {
            let on = on_off(word(1))?;
            wallet::change(|w| {
                w.policy.testnets = on;
                Ok(())
            })?;
            Ok(if on { "Test networks are on.".into() } else { "Test networks are off.".into() })
        }
        other => Err(format!("No chains command `{other}`. See letronna wallet --help.")),
    }
}

fn on_off(word: &str) -> Result<bool, String> {
    match word {
        "on" => Ok(true),
        "off" => Ok(false),
        _ => Err("Say on or off.".into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn words(all: &[&str]) -> Vec<String> {
        all.iter().map(|w| w.to_string()).collect()
    }

    /// A keyboard that answers from a script and records what it was asked.
    struct Scripted {
        lines: Vec<String>,
        picks: Vec<Option<usize>>,
        asked: Vec<String>,
    }

    impl Keyboard for Scripted {
        fn secret(&mut self, hint: &str) -> Option<String> {
            self.asked.push(hint.to_owned());
            self.lines.pop()
        }
        fn line(&mut self, hint: &str) -> Option<String> {
            self.asked.push(hint.to_owned());
            self.lines.pop()
        }
        fn choose(&mut self, question: &str, _: Vec<String>, _: &[&str]) -> Option<usize> {
            self.asked.push(question.to_owned());
            self.picks.pop().flatten()
        }
        fn busy(&mut self, _: &str, work: Work) -> Result<String, String> {
            work()
        }
    }

    fn keys(lines: &[&str], picks: &[Option<usize>]) -> Scripted {
        Scripted {
            lines: lines.iter().rev().map(|l| l.to_string()).collect(),
            picks: picks.iter().rev().copied().collect(),
            asked: Vec::new(),
        }
    }

    #[test]
    fn flags_are_taken_out_wherever_they_are_and_unknown_ones_refused() {
        let (plain, flags) =
            split_flags(&words(&["send", "1", "--from", "Main", "usdc", "--dollar"])).unwrap();
        assert_eq!(plain, ["send", "1", "usdc"]);
        assert_eq!(flags["--from"], "Main");
        assert!(flags.contains_key("--dollar"));
        assert!(
            split_flags(&words(&["--index"]))
                .unwrap_err()
                .contains("needs a value")
        );
        assert!(
            split_flags(&words(&["--everything"]))
                .unwrap_err()
                .contains("Unknown option")
        );
    }

    #[test]
    fn only_reading_runs_without_a_terminal() {
        for read in [
            "",
            "list",
            "balance Main --chain base",
            "history",
            "limits",
            "sites",
            "chains",
            "prefer",
        ] {
            assert!(
                reads_only(&words(
                    &read
                        .split(' ')
                        .filter(|w| !w.is_empty())
                        .collect::<Vec<_>>()
                )),
                "{read}"
            );
        }
        for change in [
            "send 1 usdc to 0x209693Bc6afc0C5328bA36FaF03C514EF312287C on base",
            "reveal Main",
            "import Main",
            "new",
            "remove Main",
            "limits --per-day 100",
            "x402 auto",
            "x402",
            "resume",
            "pause",
            "sites allow example.com",
            "only-saved off",
            "chains rpc base https://rpc.example",
            "save shop 0x209693Bc6afc0C5328bA36FaF03C514EF312287C",
            "default Main",
            "something-new",
        ] {
            assert!(
                !reads_only(&words(&change.split(' ').collect::<Vec<_>>())),
                "{change}"
            );
        }
    }

    #[test]
    fn a_send_written_wrong_says_its_shape_before_any_question() {
        let mut keys = keys(&[], &[Some(0)]);
        for wrong in [
            &["5", "usdc", "0xabc", "on", "base"][..],
            &["5", "usdc", "to", "0xabc"],
            &["to", "0xabc", "on", "base"],
            &["5", "usdc", "to", "0xabc", "on"],
        ] {
            let why = send(&mut keys, &words(wrong), None).unwrap_err();
            assert!(why.contains("letronna wallet send 5 usdc"), "{why}");
        }
        assert!(keys.asked.is_empty());
    }

    #[test]
    fn a_secret_is_asked_for_hidden_and_reveal_wants_the_name_typed() {
        // An empty answer brings nothing in, and nothing is kept.
        let mut empty = keys(&[""], &[]);
        assert_eq!(
            verb(&words(&["import"]), &mut empty).unwrap_err(),
            "Nothing was brought in."
        );
        assert!(empty.asked[0].contains("Recovery phrase"));
        assert!(
            verb(&words(&["reveal"]), &mut keys(&[], &[]))
                .unwrap_err()
                .contains("which wallet")
        );
    }
}
