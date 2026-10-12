//! Who is signed in.
//!
//! The flow is the ordinary one for a program with no browser of its own: it
//! opens the sign-in page in yours, listens on a port only this machine can
//! reach, and waits for the page to send the token back to it. Nothing is
//! typed and no password passes through here.
//!
//! What comes back is a device token for this computer. Who it belongs to
//! lives in `account.json`, which the terminal and the window both read, so
//! signing in or out in one is seen by the other; the token itself is kept
//! in the system's vault where there is one (see `vault`). The token is
//! checked against the API for the plan and credits, and the dashboard can
//! sign a computer out.

use std::io::{BufRead, BufReader, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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

pub(crate) fn api(path: &str) -> String {
    let base = std::env::var("LETRONNA_API").unwrap_or_else(|_| API.to_owned());
    format!("{}{path}", base.trim_end_matches('/'))
}

/// The hosted pool's OpenAI-style base URL, on the same API as the account.
pub fn hosted_base() -> String {
    api("/v1")
}

/// The account page on the website.
pub fn dashboard() -> String {
    let site = std::env::var("LETRONNA_WEB").unwrap_or_else(|_| WEBSITE.to_owned());
    format!("{}/dashboard", site.trim_end_matches('/'))
}

/// Where credit is bought on the website.
pub fn checkout() -> String {
    format!("{}/checkout", dashboard())
}

/// How long to hold the port open before giving up on the browser. Ten
/// minutes, because a first sign-in can take a GitHub login and a wait for
/// the device-verification email.
const PATIENCE: Duration = Duration::from_secs(10 * 60);

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
    /// The team this computer bills hosted models to, if one was picked.
    #[serde(default)]
    pub team: Option<Team>,
}

/// A team: its id, its name, and (for the one in use) the pool left, in cents.
#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Team {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub credits: i64,
    #[serde(default)]
    pub role: String,
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
        format!(
            "${}.{:02}",
            self.credits / 100,
            self.credits.rem_euclid(100)
        )
    }
}

pub fn path() -> PathBuf {
    backend::data_dir().join("account.json")
}

/// Where in `account.json` the token sits, and its name in the vault.
pub(crate) const SECRETS: &[(&str, &str)] = &[("/token", TOKEN)];
const TOKEN: &str = "account.token";

fn on_disk() -> Option<Account> {
    std::fs::read(path())
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
}

/// Who is signed in, if anyone. A token the vault would not give reads as
/// signed out, and `vault::trouble` says why.
pub fn account() -> Option<Account> {
    // A cloud terminal is handed its opener's sign-in for that run; what the
    // API last said of them is still kept in the file, without the token.
    if let Some(token) = given() {
        return Some(Account {
            token,
            ..on_disk().unwrap_or_default()
        });
    }
    let mut account = on_disk()?;
    if account.token.is_empty() {
        account.token = crate::vault::get(TOKEN).ok().flatten().unwrap_or_default();
    }
    (!account.token.trim().is_empty()).then_some(account)
}

/// The token `LETRONNA_TOKEN` hands in, as letronna-cloud does for the
/// terminal it opens; never written anywhere.
fn given() -> Option<String> {
    std::env::var("LETRONNA_TOKEN")
        .ok()
        .filter(|token| !token.trim().is_empty())
}

/// Forget the token here: the vault's copy first, and the file only once
/// that is gone, so a token the vault would not let go of is not left there
/// unlisted and unseen.
fn forget() -> Result<(), String> {
    crate::vault::remove(TOKEN)
        .map_err(|why| format!("Not signed out: {why}, and the sign-in is still kept here."))?;
    let _ = std::fs::remove_file(path());
    Ok(())
}

pub fn signed_in() -> bool {
    account().is_some()
}

/// Sign this computer out: on the server first when the token can be read,
/// so it drops off the dashboard's list, then here. Gives what is left to do
/// on the website when the server was not told, and an error saying what is
/// left when the token could not be taken out of the vault.
pub fn sign_out() -> Result<Option<String>, String> {
    let note = match account() {
        Some(account) => {
            let revoked = client(Duration::from_secs(5)).and_then(|c| {
                c.post(api("/devices/self/revoke"))
                    .bearer_auth(&account.token)
                    .send()
                    .map_err(|e| e.to_string())
            });
            // 401: the dashboard signed it out already.
            let told = revoked.is_ok_and(|answer| {
                answer.status().is_success() || answer.status().as_u16() == 401
            });
            (!told).then_some(
                "The account service could not be told, so sign this computer out \
                 on the website too.",
            )
        }
        None if on_disk().is_some() => Some(
            "The sign-in could not be read, so the account service was not told: \
             sign this computer out on the website too.",
        ),
        None => None,
    };
    forget()?;
    Ok(note.map(str::to_owned))
}

/// Ask the API who this token belongs to and what they have, and keep the
/// answer. A token the dashboard signed out is dropped here too.
pub fn refresh() -> Result<Account, String> {
    refresh_within(Duration::from_secs(15))
}

/// `refresh`, giving up on the API after `wait`.
pub(crate) fn refresh_within(wait: Duration) -> Result<Account, String> {
    let mut account = account().ok_or("Not signed in.")?;
    let response = client(wait)?
        .get(api("/me"))
        .bearer_auth(&account.token)
        .send()
        .map_err(|_| "Could not reach the Letronna account service.".to_owned())?;
    if response.status().as_u16() == 401 {
        if let Err(why) = forget() {
            return Err(format!(
                "This computer was signed out on the website. {why}"
            ));
        }
        return Err("This computer was signed out. /login to sign in again.".into());
    }
    #[derive(Deserialize)]
    struct Me {
        name: String,
        email: String,
        plan: String,
        credits: i64,
        #[serde(default)]
        team: Option<Team>,
    }
    let me: Me = response
        .error_for_status()
        .map_err(|e| {
            format!(
                "The account service answered {}.",
                e.status().map_or(0, |s| s.as_u16())
            )
        })?
        .json()
        .map_err(|_| "The account service sent something unexpected.".to_owned())?;
    account.name = me.name;
    account.email = me.email;
    account.plan = me.plan;
    account.credits = me.credits;
    account.team = me.team;
    remember(&account)?;
    Ok(account)
}

/// The teams the signed-in person is in.
pub fn teams() -> Result<Vec<Team>, String> {
    let account = account().ok_or("Not signed in.")?;
    client(Duration::from_secs(15))?
        .get(api("/teams"))
        .bearer_auth(&account.token)
        .send()
        .and_then(|r| r.error_for_status())
        .map_err(|_| "Could not load your teams.".to_owned())?
        .json()
        .map_err(|_| "The account service sent something unexpected.".to_owned())
}

/// Bill this computer's hosted models to a team (`Some(id)`) or to the
/// person's own credit (`None`), then keep the answer.
pub fn use_team(id: Option<&str>) -> Result<Account, String> {
    let account = account().ok_or("Not signed in.")?;
    let response = client(Duration::from_secs(20))?
        .post(api("/devices/team"))
        .bearer_auth(&account.token)
        .json(&serde_json::json!({ "org_id": id.unwrap_or("") }))
        .send()
        .map_err(|_| "Could not reach the Letronna account service.".to_owned())?;
    if !response.status().is_success() {
        #[derive(Deserialize)]
        struct Refused {
            error: String,
        }
        let why = response
            .json::<Refused>()
            .map(|r| r.error)
            .unwrap_or_else(|_| "That team could not be picked.".into());
        return Err(why);
    }
    refresh()
}

fn client(timeout: Duration) -> Result<reqwest::blocking::Client, String> {
    reqwest::blocking::Client::builder()
        .timeout(timeout)
        .build()
        .map_err(|e| e.to_string())
}

/// Keep who is signed in, with the token in the vault where there is one; a
/// vault that refuses it fails the sign-in rather than leave it in the file.
fn remember(account: &Account) -> Result<(), String> {
    let _ = std::fs::create_dir_all(backend::data_dir());
    let token = match given() {
        Some(_) => String::new(),
        None => {
            let in_file = on_disk().map(|account| account.token).unwrap_or_default();
            crate::vault::save(TOKEN, &account.token, &in_file)
                .map_err(|why| format!("Could not keep the sign-in: {why}."))?
        }
    };
    let kept = Account {
        token,
        ..account.clone()
    };
    let bytes = serde_json::to_vec_pretty(&kept).map_err(|e| e.to_string())?;
    crate::store::write_private(&path(), &bytes).map_err(|e| e.to_string())
}

/// A sign-in waiting for the browser to come back.
pub struct Pending {
    listener: TcpListener,
    /// Sent to the site and checked when it answers, so another page on the
    /// machine cannot hand us a token we did not ask for.
    state: String,
    cancel: Arc<AtomicBool>,
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
    let state = token()?;
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
        cancel: Arc::new(AtomicBool::new(false)),
        url,
    })
}

impl Pending {
    /// Wait for the browser. The page is expected to come back to the
    /// callback with `token`, `state` and who it belongs to.
    pub fn finish(self) -> Result<Account, String> {
        let account = self.wait()?;
        remember(&account)?;
        // The plan and credits, when the API answers; the sign-in stands
        // either way.
        Ok(refresh().unwrap_or(account))
    }

    /// The state the page was sent, which a sign-in code is good for alone.
    pub fn state(&self) -> String {
        self.state.clone()
    }

    /// A switch that ends the wait at once, for a Cancel button or escape.
    pub fn cancel(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    /// Take connections until one is a good callback, and hand back the
    /// account it brought.
    fn wait(&self) -> Result<Account, String> {
        // Blocking in `accept` would never look at the deadline or the
        // switch if the browser was closed without coming back.
        self.listener
            .set_nonblocking(true)
            .map_err(|e| e.to_string())?;
        let deadline = std::time::Instant::now() + PATIENCE;

        while std::time::Instant::now() < deadline {
            if self.cancel.load(Ordering::Relaxed) {
                return Err("Sign-in cancelled.".into());
            }
            let Ok((stream, _)) = self.listener.accept() else {
                // Nobody yet, or a connection that dropped before it was
                // taken; neither ends the sign-in. A fifth of a second
                // between looks still notices a cancel quickly.
                std::thread::sleep(Duration::from_millis(200));
                continue;
            };
            match self.read_callback(stream) {
                Some(account) => return Ok(account),
                // Something else knocked on the port. Keep waiting for the
                // browser rather than failing the sign-in on it.
                None => continue,
            }
        }
        Err("The sign-in page was not finished in 10 minutes. Start again from Letronna.".into())
    }

    /// Read one request, answer the browser, and take what it brought.
    fn read_callback(&self, mut stream: TcpStream) -> Option<Account> {
        // An accepted socket can come out non-blocking like the listener (it
        // does on Windows) and would drop a browser that has not written yet.
        // A browser may also open a spare connection that never writes, and
        // the real one must not wait behind it for long.
        stream.set_nonblocking(false).ok()?;
        stream.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
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
        let body = page(good);
        let _ = write!(
            stream,
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\n\
             Content-Length: {}\r\nCache-Control: no-store\r\nConnection: close\r\n\r\n{body}",
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

/// Sign in with the code the page shows when the browser cannot reach this
/// machine (over SSH, say): the API takes it once, within ten minutes, and
/// only with the `state` of the sign-in it was shown for.
pub fn redeem(state: &str, code: &str) -> Result<Account, String> {
    let response = client(Duration::from_secs(20))?
        .post(api("/devices/redeem"))
        .json(&serde_json::json!({ "state": state, "code": code.trim() }))
        .send()
        .map_err(|_| "Could not reach the Letronna account service.".to_owned())?;
    #[derive(Deserialize)]
    struct Given {
        #[serde(default)]
        token: String,
        #[serde(default)]
        name: String,
        #[serde(default)]
        email: String,
        #[serde(default)]
        error: String,
    }
    let ok = response.status().is_success();
    let given: Given = response
        .json()
        .map_err(|_| "The account service sent something unexpected.".to_owned())?;
    if !ok || given.token.is_empty() {
        return Err(if given.error.is_empty() {
            "That code did not work. Start /login again.".into()
        } else {
            given.error
        });
    }
    let account = Account {
        name: given.name,
        email: given.email,
        token: given.token,
        since: now(),
        ..Account::default()
    };
    remember(&account)?;
    Ok(refresh().unwrap_or(account))
}

/// The page left in the browser, in the website's look. Nothing on it comes
/// from elsewhere, so it shows offline too and no other site hears of it.
fn page(good: bool) -> String {
    const STYLE: &str = "body{margin:0;min-height:100vh;box-sizing:border-box;padding:24px;\
        display:grid;place-items:center;background:#0040FF;color:#F2F4FF;\
        font:300 16px/1.5 'Barlow',system-ui,sans-serif}\
        main{max-width:440px;padding:40px;border:1px solid rgba(242,244,255,.26)}\
        p{margin:0;color:rgba(242,244,255,.85)}\
        .kicker{margin-bottom:12px;font-size:11px;font-weight:400;letter-spacing:.2em;\
        text-transform:uppercase}\
        h1{margin:0 0 12px;font-size:32px;font-weight:300;line-height:1;text-transform:uppercase}";
    // Browsers mostly refuse to close a tab that no script opened, which is
    // why the page still says it can be closed.
    let words = if good {
        "<h1>Signed in.</h1><p>You can close this tab and go back to Letronna.</p>\
         <script>setTimeout(() => window.close(), 1500)</script>"
    } else {
        "<h1>That did not work.</h1><p>Start the sign-in again from Letronna.</p>"
    };
    format!(
        "<!doctype html><html lang='en'><head><meta charset='utf-8'>\
         <meta name='viewport' content='width=device-width, initial-scale=1'>\
         <title>Letronna</title><style>{STYLE}</style></head>\
         <body><main><p class='kicker'>Letronna</p>{words}</main></body></html>"
    )
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// A state no one can guess: 128 bits from the system's secure generator,
/// as hex.
fn token() -> Result<String, String> {
    use ring::rand::SecureRandom;
    // The wait takes every knock on the port for ten minutes, so a state
    // worked out from the clock and the pid would give a process on this
    // machine all that time to hand us a token we never asked for.
    let mut bytes = [0u8; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "This computer would not give a random number for the sign-in.".to_owned())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Percent-decoding, enough for the fields a callback carries.
fn unescape(text: &str) -> String {
    // Bytes first and text at the end: an accented name comes as several
    // escapes, one for each byte of its UTF-8.
    let mut out = Vec::with_capacity(text.len());
    let mut bytes = text.bytes();
    while let Some(byte) = bytes.next() {
        match byte {
            b'+' => out.push(b' '),
            b'%' => {
                let hex: String = bytes.by_ref().take(2).map(char::from).collect();
                match u8::from_str_radix(&hex, 16) {
                    Ok(byte) => out.push(byte),
                    Err(_) => out.push(b'%'),
                }
            }
            _ => out.push(byte),
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

#[cfg(test)]
mod tests {
    use super::{Account, Client, Pending, begin, page, token, unescape};
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::sync::atomic::Ordering;
    use std::time::{Duration, Instant};

    #[test]
    fn a_callback_is_read_the_way_a_browser_writes_it() {
        assert_eq!(unescape("hello+there"), "hello there");
        assert_eq!(unescape("a%40b.com"), "a@b.com");
        assert_eq!(unescape("plain"), "plain");
        // A stray percent is left alone rather than eating the rest.
        assert_eq!(unescape("100%"), "100%");
    }

    #[test]
    fn a_name_with_accents_survives_the_callback() {
        assert_eq!(unescape("Jos%C3%A9"), "José");
        assert_eq!(unescape("%E2%9C%93"), "✓");
    }

    #[test]
    fn the_state_is_different_every_time() {
        let a = token().unwrap();
        let b = token().unwrap();
        assert_eq!(a.len(), 32);
        assert_ne!(a, b);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// The port and state a sign-in sends the browser off with.
    fn callback(pending: &Pending) -> (u16, String) {
        let (_, rest) = pending.url.rsplit_once("127.0.0.1:").unwrap();
        let (port, rest) = rest.split_once("/&state=").unwrap();
        let (state, _) = rest.split_once('&').unwrap();
        (port.parse().unwrap(), state.to_owned())
    }

    /// Connect to the sign-in's port and ask for `/?{query}`, as the browser
    /// does when the page sends it back.
    fn knock(port: u16, query: &str) -> TcpStream {
        let mut stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let request = format!("GET /?{query} HTTP/1.1\r\n\r\n");
        stream.write_all(request.as_bytes()).unwrap();
        stream
    }

    /// Wait on a thread of its own and cancel after a few seconds, so a wait
    /// that never ends fails the test instead of hanging it.
    fn wait_a_little(pending: Pending) -> Result<Account, String> {
        let cancel = pending.cancel();
        let (done, result) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = done.send(pending.wait());
        });
        let result = result.recv_timeout(Duration::from_secs(10));
        cancel.store(true, Ordering::Relaxed);
        result.expect("the wait never ended")
    }

    #[test]
    fn a_silent_connection_does_not_stop_the_sign_in() {
        let pending = begin(Client::App).unwrap();
        let (port, state) = callback(&pending);
        // Opened and never written to, like a browser's spare connection.
        let _spare = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let _browser = knock(port, &format!("token=t&state={state}&name=Ada"));
        let account = wait_a_little(pending).unwrap();
        assert_eq!(account.name, "Ada");
        assert_eq!(account.token, "t");
    }

    #[test]
    fn a_browser_slow_to_ask_is_still_heard() {
        let pending = begin(Client::App).unwrap();
        let (port, state) = callback(&pending);
        let browser = TcpStream::connect(("127.0.0.1", port)).unwrap();
        let mut late = browser.try_clone().unwrap();
        std::thread::spawn(move || {
            // Only after the connection has been taken, which is when a
            // socket left non-blocking would give up on it.
            std::thread::sleep(Duration::from_millis(500));
            late.write_all(format!("GET /?token=t&state={state} HTTP/1.1\r\n\r\n").as_bytes())
        });
        assert_eq!(wait_a_little(pending).unwrap().token, "t");
    }

    #[test]
    fn cancelling_ends_the_wait_at_once() {
        let pending = begin(Client::App).unwrap();
        let cancel = pending.cancel();
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(100));
            cancel.store(true, Ordering::Relaxed);
        });
        let started = Instant::now();
        assert_eq!(wait_a_little(pending).unwrap_err(), "Sign-in cancelled.");
        assert!(started.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn a_callback_with_the_wrong_state_is_refused_and_waiting_goes_on() {
        let pending = begin(Client::App).unwrap();
        let (port, state) = callback(&pending);
        let mut wrong = knock(port, &format!("token=t&state=x{state}"));
        let _right = knock(port, &format!("token=t&state={state}"));
        let account = wait_a_little(pending).unwrap();
        assert_eq!(account.token, "t");
        let mut answer = String::new();
        wrong.read_to_string(&mut answer).unwrap();
        assert!(answer.contains("That did not work."));
    }

    #[test]
    fn the_page_left_in_the_browser_is_whole_and_fetches_nothing() {
        for (good, heading) in [(true, "Signed in."), (false, "That did not work.")] {
            let html = page(good);
            assert!(html.starts_with("<!doctype html>"));
            assert!(html.ends_with("</html>"));
            assert!(html.contains("<meta charset='utf-8'>"));
            assert!(html.contains(&format!("<h1>{heading}</h1>")));
            assert!(!html.contains("http"), "the page fetches nothing");
        }
    }
}
