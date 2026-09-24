//! Who is signed in.
//!
//! The flow is the ordinary one for a program with no browser of its own: it
//! opens the sign-in page in yours, listens on a port only this machine can
//! reach, and waits for the page to send the token back to it. Nothing is
//! typed and no password passes through here.
//!
//! What comes back is a device token for this computer. It lives in
//! `account.json`, which the terminal and the window both read, so signing in
//! or out in one is seen by the other. The token is checked against the API
//! for the plan and credits, and the dashboard can sign a computer out.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::chat::{self as backend};

/// The website the browser is sent to. `LETRONNA_WEB` points it elsewhere,
/// such as `http://localhost:5173` while working on the site.
const WEBSITE: &str = "https://letronna.ai";

/// Where the browser is sent. The callback port is added as a query, the way
/// every local-callback sign-in does it.
fn authorize() -> String {
    let site = std::env::var("LETRONNA_WEB").unwrap_or_else(|_| WEBSITE.to_owned());
    format!("{}/cli/authorize", site.trim_end_matches('/'))
}

/// The account API. `LETRONNA_API` points it elsewhere, such as
/// `http://localhost:8787` while working on it.
const API: &str = "https://api.letronna.ai";

fn api(path: &str) -> String {
    let base = std::env::var("LETRONNA_API").unwrap_or_else(|_| API.to_owned());
    format!("{}{path}", base.trim_end_matches('/'))
}

/// The account page on the website.
pub fn dashboard() -> String {
    let site = std::env::var("LETRONNA_WEB").unwrap_or_else(|_| WEBSITE.to_owned());
    format!("{}/dashboard", site.trim_end_matches('/'))
}

/// How long to hold the port open before giving up on the browser.
const PATIENCE: Duration = Duration::from_secs(120);

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Account {
    /// What to call them on screen.
    pub name: String,
    pub email: String,
    /// The token the site issued. Kept out of anything printed.
    pub token: String,
    /// When it was issued, in seconds since the epoch.
    pub since: u64,
    /// As of the last check with the API: `free`, `pro` or `team`.
    #[serde(default)]
    pub plan: String,
    /// Hosted-model credit left, in cents, as of the last check.
    #[serde(default)]
    pub credits: i64,
}

impl Account {
    /// The plan, spelled for people.
    pub fn plan_name(&self) -> &str {
        match self.plan.as_str() {
            "pro" => "Pro",
            "team" => "Team",
            "" => "",
            _ => "Free",
        }
    }

    /// Credit left, as dollars.
    pub fn credit(&self) -> String {
        format!("${}.{:02}", self.credits / 100, self.credits.rem_euclid(100))
    }
}

pub fn path() -> PathBuf {
    backend::data_dir().join("account.json")
}

/// Who is signed in, if anyone.
pub fn account() -> Option<Account> {
    let account: Account = std::fs::read(path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())?;
    (!account.token.trim().is_empty()).then_some(account)
}

pub fn signed_in() -> bool {
    account().is_some()
}

/// Sign this computer out, on the server as well when it can be reached, so
/// it drops off the dashboard's list. The local file goes either way.
pub fn sign_out() {
    if let Some(account) = account() {
        let _ = client(Duration::from_secs(5)).and_then(|c| {
            c.post(api("/devices/self/revoke"))
                .bearer_auth(&account.token)
                .send()
                .map_err(|e| e.to_string())
        });
    }
    let _ = std::fs::remove_file(path());
}

/// Ask the API who this token belongs to and what they have, and keep the
/// answer. A token the dashboard signed out is dropped here too.
pub fn refresh() -> Result<Account, String> {
    let mut account = account().ok_or("Not signed in.")?;
    let response = client(Duration::from_secs(15))?
        .get(api("/me"))
        .bearer_auth(&account.token)
        .send()
        .map_err(|_| "Could not reach the Letronna account service.".to_owned())?;
    if response.status().as_u16() == 401 {
        let _ = std::fs::remove_file(path());
        return Err("This computer was signed out. /login to sign in again.".into());
    }
    #[derive(Deserialize)]
    struct Me {
        name: String,
        email: String,
        plan: String,
        credits: i64,
    }
    let me: Me = response
        .error_for_status()
        .map_err(|e| format!("The account service answered {}.", e.status().map_or(0, |s| s.as_u16())))?
        .json()
        .map_err(|_| "The account service sent something unexpected.".to_owned())?;
    account.name = me.name;
    account.email = me.email;
    account.plan = me.plan;
    account.credits = me.credits;
    remember(&account)?;
    Ok(account)
}

fn client(timeout: Duration) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| e.to_string())
}

fn remember(account: &Account) -> Result<(), String> {
    let _ = std::fs::create_dir_all(backend::data_dir());
    let bytes = serde_json::to_vec_pretty(account).map_err(|e| e.to_string())?;
    std::fs::write(path(), bytes).map_err(|e| e.to_string())
}

/// A sign-in waiting for the browser to come back.
pub struct Pending {
    listener: TcpListener,
    /// Sent to the site and checked when it answers, so another page on the
    /// machine cannot hand us a token we did not ask for.
    state: String,
    pub url: String,
}

/// Which Letronna is signing in, so the dashboard can tell the terminal and
/// the window apart in its list of devices.
#[derive(Clone, Copy)]
pub enum Client {
    Cli,
    App,
}

/// Open the port and work out where to send the browser.
pub fn begin(client: Client) -> Result<Pending, String> {
    // Port zero asks the operating system for a free one, which avoids both
    // a clash and a fixed port for anything else to sit on.
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .map_err(|e| format!("Cannot listen for the sign-in: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let state = token(16);
    let client = match client {
        Client::Cli => "cli",
        Client::App => "app",
    };
    let url = format!(
        "{}?callback=http://127.0.0.1:{port}/&state={state}&client={client}",
        authorize()
    );
    Ok(Pending {
        listener,
        state,
        url,
    })
}

impl Pending {
    /// Wait for the browser. The page is expected to come back to the
    /// callback with `token`, `state` and who it belongs to.
    pub fn finish(self) -> Result<Account, String> {
        self.listener
            .set_nonblocking(false)
            .map_err(|e| e.to_string())?;
        let deadline = std::time::Instant::now() + PATIENCE;

        while std::time::Instant::now() < deadline {
            let Ok((stream, _)) = self.listener.accept() else {
                continue;
            };
            match self.read_callback(stream) {
                Some(account) => {
                    remember(&account)?;
                    // The plan and credits, when the API answers; the
                    // sign-in stands either way.
                    return Ok(refresh().unwrap_or(account));
                }
                // Something else knocked on the port. Keep waiting for the
                // browser rather than failing the sign-in on it.
                None => continue,
            }
        }
        Err("The sign-in was not finished in time.".into())
    }

    /// Read one request, answer the browser, and take what it brought.
    fn read_callback(&self, mut stream: TcpStream) -> Option<Account> {
        let mut line = String::new();
        BufReader::new(stream.try_clone().ok()?)
            .read_line(&mut line)
            .ok()?;
        // "GET /?token=…&state=… HTTP/1.1"
        let target = line.split_whitespace().nth(1)?;
        let query = target.split_once('?').map(|(_, q)| q).unwrap_or("");
        let mut token = String::new();
        let mut state = String::new();
        let mut name = String::new();
        let mut email = String::new();
        for pair in query.split('&') {
            let (key, value) = pair.split_once('=').unwrap_or((pair, ""));
            let value = unescape(value);
            match key {
                "token" => token = value,
                "state" => state = value,
                "name" => name = value,
                "email" => email = value,
                _ => {}
            }
        }

        let good = state == self.state && !token.is_empty();
        let body = if good {
            "<h2>Signed in.</h2><p>You can close this tab and go back to Letronna.</p>"
        } else {
            "<h2>That did not work.</h2><p>Start the sign-in again from Letronna.</p>"
        };
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        let _ = stream.flush();

        good.then(|| Account {
            name: if name.is_empty() {
                "Letronna user".into()
            } else {
                name
            },
            email,
            token,
            since: now(),
            ..Account::default()
        })
    }
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// A random-enough string. This guards a callback on this machine for the
/// next two minutes, which does not want a cryptographic generator, and
/// pulling one in for it would be the only reason to have it.
fn token(len: usize) -> String {
    const ALPHABET: &[u8] = b"abcdefghijklmnopqrstuvwxyz0123456789";
    // The clock alone is not enough: two calls in the same second start from
    // the same number and hand back the same string, which for a state is
    // the one thing it must never do. The counter carries on from wherever
    // the last call left it.
    static STATE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let mut seed = STATE.fetch_add(0x9E37_79B9_7F4A_7C15, std::sync::atomic::Ordering::Relaxed)
        ^ now()
        ^ (std::process::id() as u64) << 17;
    if seed == 0 {
        seed = 0x2545_F491_4F6C_DD1D;
    }
    (0..len)
        .map(|_| {
            // A small xorshift, which is plenty for picking letters.
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            ALPHABET[(seed % ALPHABET.len() as u64) as usize] as char
        })
        .collect()
}

/// Percent-decoding, enough for the fields a callback carries.
fn unescape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        match ch {
            '+' => out.push(' '),
            '%' => {
                let hex: String = chars.by_ref().take(2).collect();
                match u8::from_str_radix(&hex, 16) {
                    Ok(byte) => out.push(byte as char),
                    Err(_) => out.push('%'),
                }
            }
            _ => out.push(ch),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{token, unescape};

    #[test]
    fn a_callback_is_read_the_way_a_browser_writes_it() {
        assert_eq!(unescape("hello+there"), "hello there");
        assert_eq!(unescape("a%40b.com"), "a@b.com");
        assert_eq!(unescape("plain"), "plain");
        // A stray percent is left alone rather than eating the rest.
        assert_eq!(unescape("100%"), "100%");
    }

    #[test]
    fn the_state_is_different_every_time() {
        let a = token(16);
        let b = token(16);
        assert_eq!(a.len(), 16);
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_alphanumeric()));
    }
}
