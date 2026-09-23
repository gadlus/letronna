//! Who is signed in.
//!
//! The flow is the ordinary one for a program with no browser of its own: it
//! opens the sign-in page in yours, listens on a port only this machine can
//! reach, and waits for the page to send the token back to it. Nothing is
//! typed and no password passes through here.
//!
//! The site that answers does not exist yet, so `stub` fills in for it: the
//! same shape, the same callback, the same file written at the end, with the
//! token made up locally. When there is a real site to talk to, `AUTHORIZE`
//! and the exchange in `finish` are what change; everything either side of
//! them is already what it will be.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::chat::{self as backend};

/// Where the browser is sent. The callback port is added as a query, the way
/// every local-callback sign-in does it.
const AUTHORIZE: &str = "https://letronna.app/cli/authorize";

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

pub fn sign_out() {
    let _ = std::fs::remove_file(path());
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

/// Open the port and work out where to send the browser.
pub fn begin() -> Result<Pending, String> {
    // Port zero asks the operating system for a free one, which avoids both
    // a clash and a fixed port for anything else to sit on.
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .map_err(|e| format!("Cannot listen for the sign-in: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let state = token(16);
    let url = format!("{AUTHORIZE}?callback=http://127.0.0.1:{port}/&state={state}");
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
                    return Ok(account);
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
        })
    }
}

/// Sign in without a site to sign in to.
///
/// Everything above is the real flow and none of it can run until there is a
/// page at the other end. This writes the same file the real one would, so
/// the rest of the program -- the installer, the footer, `/whoami` -- is
/// exercised for real rather than waiting on a server.
pub fn stub(name: &str) -> Result<Account, String> {
    let account = Account {
        name: name.to_owned(),
        email: format!("{}@letronna.local", name.to_lowercase().replace(' ', ".")),
        token: format!("dummy-{}", token(24)),
        since: now(),
    };
    remember(&account)?;
    Ok(account)
}

/// Whether a token came from `stub` rather than from a site.
pub fn is_stub(account: &Account) -> bool {
    account.token.starts_with("dummy-")
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
