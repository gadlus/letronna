//! Previews of HTML and SVG artifacts, served to this computer's browser.
//!
//! Opening such a file from disk gives it the file: origin and no policy at
//! all. A preview instead serves the one file from 127.0.0.1 on a free port,
//! under a path with 128 random bits in it, for as long as it keeps being
//! asked for. Only that file is served, never its folder: a page the model
//! wrote could otherwise read the project around it and carry it off in a
//! link, so a stylesheet or picture beside it is not found and belongs
//! inline. The page may reach nothing but itself, and a sandbox gives it an
//! origin of its own, because cookies on 127.0.0.1 are shared by every port.
//! Whatever it can still send out, by following a link say, carries nothing
//! the file does not already hold. The Host header must name the address
//! handed out, which turns away a site whose name was pointed at 127.0.0.1.
//! Nothing ticks: one timer entry at a time waits for the page to go
//! unasked-for, and the server ends with the process too.

use std::io::{ErrorKind, Read, Write};
use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use crate::timers;

/// How long a preview lasts after the page was last asked for. Fifteen
/// minutes is time to read it and come back after the model edits it; a
/// reload starts the count again.
pub const IDLE: Duration = Duration::from_secs(15 * 60);

/// Connections answered at once. A browser opens six to a host, and one or
/// two more ahead of time; past this it is not a browser, and is closed.
const MOST_OPEN: usize = 16;

/// How long a connection may take to send its request. A browser sends it
/// at once; one opened ahead of time that never does is closed after this,
/// and holds only its own thread meanwhile.
const READ: Duration = Duration::from_secs(10);

/// How long one write may wait on a browser that stopped reading.
const WRITE: Duration = Duration::from_secs(30);

/// The most a request line and its headers may take. A browser's are a
/// kilobyte or two, but they carry every cookie other servers on 127.0.0.1
/// have set, a cookie's address having no port in it.
const HEAD: usize = 32 * 1024;

/// What the page may load: itself, and what it carries inline or as data.
/// The sandbox leaves out same-origin, so the page cannot read those shared
/// cookies, and forms go nowhere.
const POLICY: &str = "default-src 'self' 'unsafe-inline' 'unsafe-eval' data: blob:; \
                      form-action 'none'; base-uri 'none'; frame-ancestors 'none'; \
                      object-src 'none'; \
                      sandbox allow-scripts allow-modals allow-popups allow-downloads";

/// One file being previewed, and what its server needs to know.
struct Server {
    /// The file, links resolved, as it was when the preview started.
    file: PathBuf,
    /// Its Content-Type.
    kind: &'static str,
    /// The one path answered: the token, then the file's name.
    route: String,
    /// The one Host header answered: the address handed out.
    host: String,
    /// Where the server listens, for waking it to stop.
    address: SocketAddr,
    idle: Duration,
    /// When the page was last served.
    last: Mutex<Instant>,
    stopped: AtomicBool,
    /// Connections being answered.
    open: AtomicUsize,
}

/// Whether `file` is a kind that can be previewed, by its name.
pub fn previewable(file: &Path) -> bool {
    kind(file).is_some()
}

/// Serve `file`, an HTML or SVG file, until nobody has asked for it in
/// `IDLE`, and give the address that shows it.
pub fn start(file: &Path) -> Result<String, String> {
    serve(file, IDLE)
}

/// `start`, going quiet after `idle` instead.
fn serve(file: &Path, idle: Duration) -> Result<String, String> {
    let shown = file.display();
    let file = std::fs::canonicalize(file).map_err(|e| format!("Cannot open {shown}: {e}"))?;
    let Some(kind) = kind(&file) else {
        return Err(format!(
            "{shown} is not an HTML or SVG file, so there is nothing to preview."
        ));
    };
    if !file.is_file() {
        return Err(format!("{shown} is not a file."));
    }
    if crate::tools::secret(&file) || crate::tools::private(&file, &crate::chat::data_dir()) {
        return Err(format!(
            "{shown} is where keys, passwords or tokens are kept, so it is not served."
        ));
    }
    let listener = TcpListener::bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))
        .map_err(|e| format!("Could not start a preview on this computer: {e}"))?;
    let address = listener
        .local_addr()
        .map_err(|e| format!("Could not start a preview on this computer: {e}"))?;
    let name = file
        .file_name()
        .map(|name| escape(&name.to_string_lossy()))
        .unwrap_or_default();
    let route = format!("/{}/{name}", token()?);
    let host = address.to_string();
    let url = format!("http://{host}{route}");
    let server = Arc::new(Server {
        file,
        kind,
        route,
        host,
        address,
        idle,
        last: Mutex::new(Instant::now()),
        stopped: AtomicBool::new(false),
        open: AtomicUsize::new(0),
    });
    let accepting = server.clone();
    std::thread::Builder::new()
        .name("preview".to_owned())
        .spawn(move || accept(listener, accepting))
        .map_err(|e| format!("Could not start a preview on this computer: {e}"))?;
    arm(server, Instant::now() + idle);
    Ok(url)
}

/// Answer connections, each on a thread of its own, until the preview stops.
fn accept(listener: TcpListener, server: Arc<Server>) {
    for stream in listener.incoming() {
        if server.stopped.load(Ordering::SeqCst) {
            break;
        }
        let stream = match stream {
            Ok(stream) => stream,
            // A browser that gave up before it was let in; Windows says so
            // as a reset.
            Err(e)
                if matches!(
                    e.kind(),
                    ErrorKind::ConnectionAborted
                        | ErrorKind::ConnectionReset
                        | ErrorKind::Interrupted
                ) =>
            {
                continue;
            }
            // Anything else, such as running out of open files, would come
            // straight back and spin, so the preview ends instead.
            Err(_) => break,
        };
        let Some(slot) = Slot::take(&server) else {
            continue;
        };
        let _ = std::thread::Builder::new()
            .name("preview-answer".to_owned())
            .spawn(move || answer(stream, &slot.0));
    }
    server.stopped.store(true, Ordering::SeqCst);
}

/// A place among the `MOST_OPEN` connections, given back when dropped: when
/// the answer is sent, fails or panics, or its thread never starts.
struct Slot(Arc<Server>);

impl Slot {
    fn take(server: &Arc<Server>) -> Option<Slot> {
        let before = server.open.fetch_add(1, Ordering::SeqCst);
        let slot = Slot(server.clone());
        (before < MOST_OPEN).then_some(slot)
    }
}

impl Drop for Slot {
    fn drop(&mut self) {
        self.0.open.fetch_sub(1, Ordering::SeqCst);
    }
}

/// Read one request and answer it with the file, or with why not.
fn answer(mut stream: TcpStream, server: &Server) {
    let _ = stream.set_read_timeout(Some(READ));
    let _ = stream.set_write_timeout(Some(WRITE));
    let Some(request) = read_head(&mut stream) else {
        return;
    };
    let _ = match refusal(&request, server) {
        Some(status) => refuse(&mut stream, status),
        None => send(&mut stream, server),
    };
}

/// The request line and headers, or None for a connection that sends none
/// in time, sends too much or sends something other than text.
fn read_head(stream: &mut TcpStream) -> Option<String> {
    let started = Instant::now();
    let mut head = Vec::new();
    let mut chunk = [0u8; 4096];
    let end = loop {
        if let Some(end) = head.windows(4).position(|four| four == b"\r\n\r\n") {
            break end;
        }
        if head.len() > HEAD || started.elapsed() > READ {
            return None;
        }
        let read = stream.read(&mut chunk).ok().filter(|&read| read > 0)?;
        head.extend_from_slice(&chunk[..read]);
    };
    head.truncate(end);
    String::from_utf8(head).ok()
}

/// The status `head` is refused with, or None when it asks for the page the
/// way the address handed out does.
fn refusal(head: &str, server: &Server) -> Option<&'static str> {
    let mut lines = head.split("\r\n");
    let mut request = lines.next().unwrap_or_default().split(' ');
    let method = request.next().unwrap_or_default();
    let target = request.next().unwrap_or_default();
    let hosts: Vec<&str> = lines
        .filter_map(|line| line.split_once(':'))
        .filter(|(name, _)| name.eq_ignore_ascii_case("host"))
        .map(|(_, value)| value.trim())
        .collect();
    // First, so a site whose name was pointed here learns nothing at all,
    // not even which methods there are.
    if hosts != [server.host.as_str()] {
        return Some("403 Forbidden");
    }
    if method != "GET" {
        return Some("405 Method Not Allowed");
    }
    let path = target.split(['?', '#']).next().unwrap_or_default();
    if !same(path.as_bytes(), server.route.as_bytes()) {
        return Some("404 Not Found");
    }
    None
}

/// Send the file, if it is still the one the preview started with.
fn send(stream: &mut TcpStream, server: &Server) -> std::io::Result<()> {
    // A link put where the file was would lead somewhere else, perhaps to a
    // secret, so the path must still resolve to itself.
    let unmoved = std::fs::canonicalize(&server.file).is_ok_and(|now| now == server.file);
    let opened = std::fs::File::open(&server.file)
        .and_then(|file| Ok((file.metadata()?, file)))
        .ok()
        .filter(|(meta, _)| unmoved && meta.is_file());
    let Some((meta, file)) = opened else {
        return refuse(stream, "404 Not Found");
    };
    *server.last.lock().unwrap_or_else(PoisonError::into_inner) = Instant::now();
    stream.write_all(head("200 OK", server.kind, meta.len()).as_bytes())?;
    // No further than the length promised, should the file grow meanwhile.
    std::io::copy(&mut file.take(meta.len()), stream)?;
    stream.flush()
}

/// Answer with `status` and its reason, and nothing else.
fn refuse(stream: &mut TcpStream, status: &str) -> std::io::Result<()> {
    let why = status.split_once(' ').map_or(status, |(_, why)| why);
    let text = "text/plain; charset=utf-8";
    stream.write_all(head(status, text, why.len() as u64).as_bytes())?;
    stream.write_all(why.as_bytes())
}

/// The status line and headers every reply has: the policy, no guessing at
/// types, no address given to links followed out, and nothing cached.
fn head(status: &str, kind: &str, length: u64) -> String {
    format!(
        "HTTP/1.1 {status}\r\nContent-Type: {kind}\r\nContent-Length: {length}\r\n\
         Content-Security-Policy: {POLICY}\r\nX-Content-Type-Options: nosniff\r\n\
         Referrer-Policy: no-referrer\r\nCross-Origin-Resource-Policy: same-origin\r\n\
         X-Frame-Options: DENY\r\nCache-Control: no-store\r\nAllow: GET\r\n\
         Connection: close\r\n\r\n"
    )
}

/// Whether `a` is `b`, taking as long wherever they first differ, so the
/// time a refusal takes gives nothing of the token away.
fn same(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0, |diff, (x, y)| diff | (x ^ y)) == 0
}

/// The Content-Type `file` is served as, if it is a kind that is previewed.
fn kind(file: &Path) -> Option<&'static str> {
    let extension = file.extension()?.to_string_lossy().to_ascii_lowercase();
    match extension.as_str() {
        "html" | "htm" => Some("text/html; charset=utf-8"),
        "svg" => Some("image/svg+xml"),
        _ => None,
    }
}

/// `name` with all but letters, digits and `-._~` percent-escaped, so it is
/// one path segment, and the same one a browser asks for, however it is
/// spelled.
fn escape(name: &str) -> String {
    name.bytes()
        .map(|byte| match byte {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                char::from(byte).to_string()
            }
            _ => format!("%{byte:02X}"),
        })
        .collect()
}

/// 128 bits from the system's secure generator, as hex: the part of the
/// address nobody can guess.
fn token() -> Result<String, String> {
    use ring::rand::SecureRandom;
    let mut bytes = [0u8; 16];
    ring::rand::SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| "This computer would not give a random number for the preview.".to_owned())?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

/// Look at `when` whether the page has gone unasked-for.
fn arm(server: Arc<Server>, when: Instant) {
    timers::at(when, Box::new(move || expire(server)));
}

/// Stop the preview if the page went unasked-for through its idle time, or
/// look again when that time would be up.
fn expire(server: Arc<Server>) {
    let last = *server.last.lock().unwrap_or_else(PoisonError::into_inner);
    let until = last + server.idle;
    if Instant::now() < until {
        return arm(server, until);
    }
    server.stopped.store(true, Ordering::SeqCst);
    // The accept loop wakes only for a connection, so it is given one, from
    // a thread of its own since a timer callback must not wait.
    let _ = std::thread::Builder::new()
        .name("preview-stop".to_owned())
        .spawn(move || {
            let _ = TcpStream::connect_timeout(&server.address, READ);
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A folder of its own under the system's temporary one.
    fn folder(name: &str) -> PathBuf {
        let dir =
            std::env::temp_dir().join(format!("letronna-preview-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    /// A page previewed from a fresh folder, with the folder and the address.
    fn page(name: &str, idle: Duration) -> (PathBuf, String) {
        let dir = folder(name);
        let file = dir.join("page.html");
        std::fs::write(&file, "<h1>made</h1>").unwrap();
        let url = serve(&file, idle).unwrap();
        (dir, url)
    }

    /// The address and the path of `url`.
    fn parts(url: &str) -> (String, String) {
        let (address, path) = url
            .strip_prefix("http://")
            .unwrap()
            .split_once('/')
            .unwrap();
        (address.to_owned(), format!("/{path}"))
    }

    /// The whole reply to `method path`, sent with each of `hosts`.
    fn ask(url: &str, method: &str, path: &str, hosts: &[&str]) -> String {
        let mut stream = TcpStream::connect(parts(url).0).unwrap();
        let hosts: String = hosts
            .iter()
            .map(|host| format!("Host: {host}\r\n"))
            .collect();
        write!(
            stream,
            "{method} {path} HTTP/1.1\r\n{hosts}Connection: close\r\n\r\n"
        )
        .unwrap();
        let mut reply = String::new();
        stream.read_to_string(&mut reply).unwrap();
        reply
    }

    /// The reply to a browser's GET of `path` at the address handed out.
    fn get(url: &str, path: &str) -> String {
        ask(url, "GET", path, &[&parts(url).0])
    }

    fn status(reply: &str) -> &str {
        reply.split("\r\n").next().unwrap()
    }

    #[test]
    fn the_page_is_served_only_under_its_token() {
        let (dir, url) = page("token", IDLE);
        let (address, path) = parts(&url);
        assert!(address.starts_with("127.0.0.1:"), "{url}");
        let token = path.split('/').nth(1).unwrap();
        assert_eq!(token.len(), 32);

        let reply = get(&url, &path);
        assert_eq!(status(&reply), "HTTP/1.1 200 OK");
        assert!(reply.ends_with("\r\n\r\n<h1>made</h1>"), "{reply}");
        // A query is the page's own business.
        assert_eq!(
            status(&get(&url, &format!("{path}?x=1"))),
            "HTTP/1.1 200 OK"
        );

        let other = format!(
            "{}{}",
            &token[..31],
            if token.ends_with('0') { '1' } else { '0' }
        );
        for wrong in [
            "/".to_owned(),
            "/page.html".to_owned(),
            format!("/{token}"),
            format!("/{token}/"),
            format!("/{other}/page.html"),
            format!("/{}/page.html", token.to_uppercase()),
        ] {
            assert_eq!(
                status(&get(&url, &wrong)),
                "HTTP/1.1 404 Not Found",
                "{wrong}"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn nothing_beside_or_above_the_page_is_served() {
        let (dir, url) = page("climb", IDLE);
        std::fs::write(dir.join("style.css"), "h1{}").unwrap();
        std::fs::write(dir.join("other.html"), "<p>other</p>").unwrap();
        let (_, path) = parts(&url);
        let token = path.split('/').nth(1).unwrap();
        for wrong in [
            format!("/{token}/style.css"),
            format!("/{token}/other.html"),
            format!("/{token}/../page.html"),
            format!("/{token}/./page.html"),
            format!("/{token}/%2e%2e/other.html"),
            format!("/{token}/page.html/../other.html"),
            format!("/{token}/page.html/"),
            format!("/{token}//page.html"),
            format!("/{token}/page.html%00.txt"),
        ] {
            let reply = get(&url, &wrong);
            assert_eq!(status(&reply), "HTTP/1.1 404 Not Found", "{wrong}");
            assert!(
                !reply.contains("other") && !reply.contains("h1{}"),
                "{reply}"
            );
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_host_other_than_the_address_handed_out_is_refused() {
        let (dir, url) = page("host", IDLE);
        let (address, path) = parts(&url);
        let port = address.rsplit(':').next().unwrap();
        let localhost = format!("localhost:{port}");
        let rebound = format!("attacker.example:{port}");
        for hosts in [
            vec![],
            vec!["attacker.example"],
            vec![rebound.as_str()],
            vec![localhost.as_str()],
            vec!["127.0.0.1"],
            vec![address.as_str(), rebound.as_str()],
            vec![rebound.as_str(), address.as_str()],
        ] {
            let reply = ask(&url, "GET", &path, &hosts);
            assert_eq!(status(&reply), "HTTP/1.1 403 Forbidden", "{hosts:?}");
            assert!(!reply.contains("made"), "{reply}");
        }
        assert_eq!(
            status(&ask(&url, "GET", &path, &[&address])),
            "HTTP/1.1 200 OK"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn only_get_is_answered() {
        let (dir, url) = page("method", IDLE);
        let (address, path) = parts(&url);
        for method in [
            "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS", "get", "TRACE",
        ] {
            let reply = ask(&url, method, &path, &[&address]);
            assert_eq!(
                status(&reply),
                "HTTP/1.1 405 Method Not Allowed",
                "{method}"
            );
            assert!(reply.contains("\r\nAllow: GET\r\n"), "{reply}");
            assert!(!reply.contains("made"), "{reply}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn every_reply_carries_the_strict_headers_and_no_cors() {
        let (dir, url) = page("headers", IDLE);
        let drawing = dir.join("drawing.SVG");
        std::fs::write(&drawing, "<svg xmlns='http://www.w3.org/2000/svg'/>").unwrap();
        let svg = serve(&drawing, IDLE).unwrap();
        let (address, path) = parts(&url);
        let replies = [
            get(&url, &path),
            get(&svg, &parts(&svg).1),
            get(&url, "/"),
            ask(&url, "OPTIONS", &path, &[&address]),
            ask(&url, "GET", &path, &["attacker.example"]),
        ];
        assert!(replies[0].contains("\r\nContent-Type: text/html; charset=utf-8\r\n"));
        assert!(replies[1].contains("\r\nContent-Type: image/svg+xml\r\n"));
        for reply in replies {
            let (head, _) = reply.split_once("\r\n\r\n").unwrap();
            assert!(head.contains(&format!("\r\nContent-Security-Policy: {POLICY}\r\n")));
            assert!(POLICY.starts_with("default-src 'self' "));
            assert!(POLICY.contains("sandbox allow-scripts") && !POLICY.contains("same-origin"));
            assert!(head.contains("\r\nX-Content-Type-Options: nosniff\r\n"));
            assert!(head.contains("\r\nReferrer-Policy: no-referrer\r\n"));
            assert!(head.contains("\r\nCache-Control: no-store\r\n"));
            assert!(
                !head.to_ascii_lowercase().contains("access-control-"),
                "{head}"
            );
            assert!(!head.to_ascii_lowercase().contains("set-cookie"), "{head}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_preview_nobody_asks_for_stops() {
        let (dir, url) = page("idle", Duration::from_millis(300));
        let (address, path) = parts(&url);
        assert_eq!(status(&get(&url, &path)), "HTTP/1.1 200 OK");
        let deadline = Instant::now() + Duration::from_secs(10);
        while TcpStream::connect(&address).is_ok() {
            assert!(Instant::now() < deadline, "the preview is still listening");
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn secret_places_and_other_kinds_of_file_are_not_served() {
        let dir = folder("secret");
        for secret in [".ssh/page.html", ".aws/drawing.svg", ".config/gh/hosts.yml"] {
            let file = dir.join(secret);
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            std::fs::write(&file, "<p>key</p>").unwrap();
            assert!(serve(&file, IDLE).is_err(), "{secret}");
        }
        std::fs::write(dir.join("notes.txt"), "plain").unwrap();
        std::fs::create_dir_all(dir.join("folder.html")).unwrap();
        for refused in ["notes.txt", "folder.html", "missing.html"] {
            assert!(serve(&dir.join(refused), IDLE).is_err(), "{refused}");
        }
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn previewable_goes_by_the_name() {
        for yes in ["a.html", "b.HTM", "c.svg"] {
            assert!(previewable(Path::new(yes)), "{yes}");
        }
        for no in ["a.txt", "b.js", "svg", "c.html.bak"] {
            assert!(!previewable(Path::new(no)), "{no}");
        }
    }

    #[test]
    fn a_name_is_escaped_to_one_path_segment() {
        assert_eq!(escape("report 2.html"), "report%202.html");
        assert_eq!(escape("a/b?c#d%.svg"), "a%2Fb%3Fc%23d%25.svg");
        assert_eq!(escape("é.html"), "%C3%A9.html");
    }

    #[cfg(unix)]
    #[test]
    fn a_page_swapped_for_a_link_is_not_served() {
        let (dir, url) = page("link", IDLE);
        let secret = dir.join(".ssh/id_ed25519");
        std::fs::create_dir_all(secret.parent().unwrap()).unwrap();
        std::fs::write(&secret, "private key").unwrap();
        let path = parts(&url).1;
        std::fs::remove_file(dir.join("page.html")).unwrap();
        std::os::unix::fs::symlink(&secret, dir.join("page.html")).unwrap();
        let reply = get(&url, &path);
        assert_eq!(status(&reply), "HTTP/1.1 404 Not Found");
        assert!(!reply.contains("private key"), "{reply}");
        let _ = std::fs::remove_dir_all(dir);
    }
}
