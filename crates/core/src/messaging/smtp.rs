//! Scheduled runs' reports by mail, through the user's own provider.
//!
//! Send-only, like the Discord webhook: nothing is read back, so a forged
//! sender, which a mailbox cannot tell from the real one without DKIM and
//! DMARC checks, has no way in. A small client rather than a mail crate,
//! since a report needs only EHLO, STARTTLS, AUTH PLAIN, MAIL, RCPT and
//! DATA, over the rustls reqwest already uses.
//!
//! The password never crosses the network readable: port 465 is TLS from
//! the first byte, and on any other port the server must offer STARTTLS or
//! nothing is sent. Gmail, iCloud, Yahoo and Fastmail take an app
//! password; Outlook and Microsoft 365 take only OAuth, so they cannot be
//! used.

use super::Email;
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use rustls::{ClientConfig, ClientConnection, StreamOwned};
use std::io::{Read, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

/// The port where TLS starts with the first byte (RFC 8314); any other, as
/// 587, starts plain and switches with STARTTLS.
const IMPLICIT_TLS: u16 = 465;
/// How long each of the server's addresses is given to answer.
const CONNECT: Duration = Duration::from_secs(15);
/// How long a reply may take. Submission servers answer at once but may
/// scan a mail before taking it, and a report goes out after its run has
/// ended, so a slow server holds nothing up.
const REPLY: Duration = Duration::from_secs(60);
/// RFC 5321 caps a reply line at 512 bytes; this allows twice that, and a
/// longer one is not a mail server talking.
const LINE_MAX: usize = 1024;
/// A capability list, the longest reply, runs to about a dozen lines.
const LINES_MAX: usize = 100;
/// The greeting: the client names itself by an address literal, which
/// servers that turn away a bare "localhost" accept.
const HELLO: &str = "EHLO [127.0.0.1]";
/// As much of a server's reply as an error repeats: its reason, and the
/// help link a provider adds.
const SAID: usize = 300;
/// A header line may run to 998 characters (RFC 5322); an ASCII subject up
/// to this goes as written, with room for the field's name.
const PLAIN_HEADER: usize = 900;
/// An RFC 2047 encoded word is 75 characters at most: 45 bytes are 60 in
/// base64, inside the 12 of "=?UTF-8?B?" and "?=".
const WORD_BYTES: usize = 45;
/// A base64 body line is 76 characters at most (RFC 2045).
const BODY_LINE: usize = 76;

/// Send a test mail as `email` says, so a wrong server, port, address or
/// password shows before the settings are saved.
pub fn check(email: &Email) -> Result<(), String> {
    check_via(tls().ok_or("Cannot initialize connection.")?, email)
}

fn check_via(tls: &Arc<ClientConfig>, email: &Email) -> Result<(), String> {
    send(
        tls,
        email,
        "Letronna can reach you",
        "Scheduled runs will send their reports to this address.",
    )
}

/// The TLS every mail goes over: the web's roots, which reqwest trusts too,
/// through ring, as reqwest uses, rather than whichever provider the process
/// would pick.
pub(super) fn tls() -> Option<&'static Arc<ClientConfig>> {
    static TLS: OnceLock<Option<Arc<ClientConfig>>> = OnceLock::new();
    TLS.get_or_init(|| {
        let roots: rustls::RootCertStore = webpki_roots::TLS_SERVER_ROOTS.iter().cloned().collect();
        let provider = Arc::new(rustls::crypto::ring::default_provider());
        let config = ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .ok()?
            .with_root_certificates(roots)
            .with_no_client_auth();
        Some(Arc::new(config))
    })
    .as_ref()
}

/// Send `body` as a plain-text mail titled `subject`, to the one address
/// `email` names.
pub(super) fn send(
    tls: &Arc<ClientConfig>,
    email: &Email,
    subject: &str,
    body: &str,
) -> Result<(), String> {
    deliver(tls, email, email.port == IMPLICIT_TLS, subject, body)
}

/// As `send`, with TLS from the first byte when `implicit`, else by
/// STARTTLS.
fn deliver(
    tls: &Arc<ClientConfig>,
    email: &Email,
    implicit: bool,
    subject: &str,
    body: &str,
) -> Result<(), String> {
    let (host, username, password) = (
        email.smtp_host.trim(),
        email.username.trim(),
        email.password.trim(),
    );
    let missing = [
        (host, "the mail server, such as smtp.gmail.com"),
        (username, "the user name, usually the email address"),
        (password, "an app password from the mail provider"),
        (email.to.trim(), "the address the reports go to"),
    ]
    .into_iter()
    .find(|(value, _)| value.is_empty());
    if let Some((_, field)) = missing {
        return Err(format!("Fill in {field}."));
    }
    if email.port == 0 {
        return Err("Choose port 465, or 587.".into());
    }
    let from = address(match email.from.trim() {
        "" => username,
        from => from,
    })?;
    let to = address(&email.to)?;
    let mail = message(from, to, subject, body);

    let mut session = Session::open(host, email.port)?;
    if implicit {
        session = session.secure(tls)?;
    }
    session.expect(2, "the connection")?;
    let offered = session.command(HELLO, 2, "the greeting")?;
    if !implicit {
        if !offered.iter().any(|line| keyword(line, "STARTTLS")) {
            return Err(format!(
                "{host} does not offer TLS on port {}, so the password would cross the \
                 network readable. Use port 465, or 587.",
                email.port
            ));
        }
        session.command("STARTTLS", 2, "TLS")?;
        session = session.secure(tls)?;
        session.command(HELLO, 2, "the greeting")?;
    }
    let login = STANDARD.encode(format!("\0{username}\0{password}"));
    session.send(&format!("AUTH PLAIN {login}"))?;
    let (code, said) = session.reply()?;
    if code != 235 {
        return Err(format!(
            "{host} refused the login ({code} {}). Most providers want an app password \
             rather than the account's own; Outlook and Microsoft 365 take only OAuth, \
             which Letronna cannot do.",
            short(&said)
        ));
    }
    session.command(&format!("MAIL FROM:<{from}>"), 2, "the sender")?;
    session.command(&format!("RCPT TO:<{to}>"), 2, "the address")?;
    session.command("DATA", 3, "the mail")?;
    session.command(&format!("{mail}\r\n."), 2, "the mail")?;
    // The mail is taken; a server that hangs up before answering this has
    // lost nothing.
    let _ = session.send("QUIT");
    Ok(())
}

/// `text` as a mail address, or why it is not one. Only the bare form, as
/// name@example.com, which has nothing to break out of the command or the
/// header it goes in.
fn address(text: &str) -> Result<&str, String> {
    let text = text.trim();
    let parts = text
        .split_once('@')
        .is_some_and(|(local, domain)| !local.is_empty() && !domain.is_empty());
    let clean = !text
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || "<>,;:\"()[]\\".contains(c));
    match parts && clean {
        true => Ok(text),
        false => Err(format!(
            "\"{text}\" is not an email address of the plain kind, as name@example.com is."
        )),
    }
}

/// The mail as DATA carries it: lines ending in CRLF, the last without
/// one. The text goes in base64, so no line of it runs past the limit and
/// none starts with the dot that would end the mail early; the headers
/// start with their names, or a fold's space.
fn message(from: &str, to: &str, subject: &str, body: &str) -> String {
    // Others can read a mailbox, as they can a chat, so what Letronna holds
    // is hidden here too.
    let text = super::outgoing(body)
        .replace("\r\n", "\n")
        .replace('\n', "\r\n");
    let mut lines = vec![
        format!("Date: {}", chrono::Local::now().to_rfc2822()),
        format!("From: {from}"),
        format!("To: {to}"),
        format!("Subject: {}", header_text(&super::outgoing(subject))),
        "MIME-Version: 1.0".to_owned(),
        "Content-Type: text/plain; charset=utf-8".to_owned(),
        "Content-Transfer-Encoding: base64".to_owned(),
        // A machine wrote it (RFC 3834), so out-of-office replies leave it be.
        "Auto-Submitted: auto-generated".to_owned(),
        String::new(),
    ];
    let encoded = STANDARD.encode(text);
    lines.extend(
        encoded
            .as_bytes()
            .chunks(BODY_LINE)
            .map(|chunk| String::from_utf8_lossy(chunk).into_owned()),
    );
    lines.join("\r\n")
}

/// `text` fit for a header: on its one line, which a job's name could
/// otherwise break to add headers of its own, and in RFC 2047 encoded words
/// when it is not plain ASCII or would make the line too long.
fn header_text(text: &str) -> String {
    let text: String = text
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    if text.is_ascii() && text.len() <= PLAIN_HEADER {
        return text;
    }
    let mut words = Vec::new();
    let mut rest = text.as_str();
    while !rest.is_empty() {
        // Whole characters to a word, as RFC 2047 asks.
        let mut end = rest.len().min(WORD_BYTES);
        while !rest.is_char_boundary(end) {
            end -= 1;
        }
        let (word, after) = rest.split_at(end);
        words.push(format!("=?UTF-8?B?{}?=", STANDARD.encode(word)));
        rest = after;
    }
    // A word to a line; a reader joins them without the space between.
    words.join("\r\n ")
}

/// Whether a capability line starts with `word`.
fn keyword(line: &str, word: &str) -> bool {
    line.split_whitespace()
        .next()
        .is_some_and(|first| first.eq_ignore_ascii_case(word))
}

/// A reply's lines as one, cut to what an error repeats.
fn short(lines: &[String]) -> String {
    lines.join(" ").chars().take(SAID).collect()
}

/// The connection to a mail server: plain until TLS is started.
enum Wire {
    Plain(TcpStream),
    Tls(Box<StreamOwned<ClientConnection, TcpStream>>),
}

impl Read for Wire {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self {
            Wire::Plain(tcp) => tcp.read(buf),
            Wire::Tls(tls) => tls.read(buf),
        }
    }
}

impl Write for Wire {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        match self {
            Wire::Plain(tcp) => tcp.write(buf),
            Wire::Tls(tls) => tls.write(buf),
        }
    }

    fn flush(&mut self) -> std::io::Result<()> {
        match self {
            Wire::Plain(tcp) => tcp.flush(),
            Wire::Tls(tls) => tls.flush(),
        }
    }
}

/// One conversation with a mail server.
struct Session {
    host: String,
    wire: Wire,
    /// What was read past the end of the last line.
    held: Vec<u8>,
}

impl Session {
    fn open(host: &str, port: u16) -> Result<Session, String> {
        let unreachable = || format!("Cannot reach {host} on port {port}.");
        let tcp = (host, port)
            .to_socket_addrs()
            .map_err(|_| format!("Cannot find the mail server {host}."))?
            .find_map(|address| TcpStream::connect_timeout(&address, CONNECT).ok())
            .ok_or_else(unreachable)?;
        tcp.set_read_timeout(Some(REPLY))
            .and_then(|()| tcp.set_write_timeout(Some(REPLY)))
            .map_err(|_| unreachable())?;
        Ok(Session {
            host: host.to_owned(),
            wire: Wire::Plain(tcp),
            held: Vec::new(),
        })
    }

    /// The same conversation over TLS, the server's certificate checked
    /// against its name.
    fn secure(self, tls: &Arc<ClientConfig>) -> Result<Session, String> {
        let host = self.host;
        let tcp = match self.wire {
            Wire::Plain(tcp) => tcp,
            Wire::Tls(_) => return Err(format!("TLS with {host} was started twice.")),
        };
        // Anything already waiting came before TLS, where whoever is on the
        // way could have put it, to be read as the server's once TLS is on.
        if !self.held.is_empty() {
            return Err(format!("{host} sent more than it should before TLS."));
        }
        let failed = |why: String| format!("Cannot open a secure connection to {host} ({why}).");
        let name = rustls::pki_types::ServerName::try_from(host.clone())
            .map_err(|error| failed(error.to_string()))?;
        let connection = ClientConnection::new(Arc::clone(tls), name)
            .map_err(|error| failed(error.to_string()))?;
        let mut stream = StreamOwned::new(connection, tcp);
        // Finished here, so a certificate that does not check out is said as
        // one rather than as a failed read.
        while stream.conn.is_handshaking() {
            stream
                .conn
                .complete_io(&mut stream.sock)
                .map_err(|error| failed(error.to_string()))?;
        }
        Ok(Session {
            host,
            wire: Wire::Tls(Box::new(stream)),
            held: Vec::new(),
        })
    }

    fn send(&mut self, line: &str) -> Result<(), String> {
        self.wire
            .write_all(format!("{line}\r\n").as_bytes())
            .and_then(|()| self.wire.flush())
            .map_err(|error| self.broken(&error))
    }

    /// Send `line`, and read a reply in the `want` hundreds: 2 for done, 3
    /// for go on. `what` says what was refused otherwise.
    fn command(&mut self, line: &str, want: u16, what: &str) -> Result<Vec<String>, String> {
        self.send(line)?;
        self.expect(want, what)
    }

    fn expect(&mut self, want: u16, what: &str) -> Result<Vec<String>, String> {
        let (code, lines) = self.reply()?;
        if code / 100 != want {
            return Err(format!(
                "{} refused {what} ({code} {}).",
                self.host,
                short(&lines)
            ));
        }
        Ok(lines)
    }

    /// The next reply: its code, and its lines without the code.
    fn reply(&mut self) -> Result<(u16, Vec<String>), String> {
        let mut lines = Vec::new();
        loop {
            let line = self.line()?;
            let code = line
                .get(..3)
                .and_then(|code| code.parse::<u16>().ok())
                .filter(|_| lines.len() < LINES_MAX)
                .ok_or_else(|| format!("{} does not answer as a mail server does.", self.host))?;
            lines.push(line.get(4..).unwrap_or_default().trim().to_owned());
            // "250-" goes on to another line; "250 ", or a bare "250", ends it.
            if line.as_bytes().get(3) != Some(&b'-') {
                return Ok((code, lines));
            }
        }
    }

    fn line(&mut self) -> Result<String, String> {
        loop {
            if let Some(end) = self.held.iter().position(|&byte| byte == b'\n') {
                let line: Vec<u8> = self.held.drain(..=end).collect();
                return Ok(String::from_utf8_lossy(&line).trim_end().to_owned());
            }
            if self.held.len() > LINE_MAX {
                return Err(format!(
                    "{} does not answer as a mail server does.",
                    self.host
                ));
            }
            let mut block = [0; 4096];
            let read = self
                .wire
                .read(&mut block)
                .map_err(|error| self.broken(&error))?;
            if read == 0 {
                return Err(format!("{} closed the connection.", self.host));
            }
            self.held.extend_from_slice(&block[..read]);
        }
    }

    fn broken(&self, error: &std::io::Error) -> String {
        use std::io::ErrorKind;
        match error.kind() {
            ErrorKind::WouldBlock | ErrorKind::TimedOut => {
                format!("{} stopped answering.", self.host)
            }
            _ => format!("{} closed the connection.", self.host),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::{Config, Email, report_via};
    use super::{Arc, ClientConfig, STANDARD, check_via, deliver, header_text};
    use base64::Engine;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};

    /// A certificate for 127.0.0.1, signed by itself and good until 2126, in
    /// base64 DER. It was made for these tests alone, so its key is no secret.
    const CERT: &str = "\
        MIIBkTCCATegAwIBAgIUe0na2YGSx/LUkUY+9ko6khRVhU8wCgYIKoZIzj0EAwIw\
        FDESMBAGA1UEAwwJMTI3LjAuMC4xMCAXDTI2MTAwMzEyNDg1N1oYDzIxMjYwOTA5\
        MTI0ODU3WjAUMRIwEAYDVQQDDAkxMjcuMC4wLjEwWTATBgcqhkjOPQIBBggqhkjO\
        PQMBBwNCAASTpP8eb6+0DZb840yaDpy8hqyg0nayWQtZrss/WF5aR2CbsNb40NlB\
        B8HSk58AVWQCh6knDInXJkZuua2ESQXvo2UwYzAPBgNVHREECDAGhwR/AAABMAwG\
        A1UdEwEB/wQCMAAwDgYDVR0PAQH/BAQDAgeAMBMGA1UdJQQMMAoGCCsGAQUFBwMB\
        MB0GA1UdDgQWBBTji5oSy2KHF8Vm5+xEjGCOxchOqDAKBggqhkjOPQQDAgNIADBF\
        AiEA4/oLaqJjFWW5vulQiUrjR0KNDH80zW8GP5wH/nY1x9ICIHRWB0Y6nQB+YLo3\
        EaZMDA2Y3JujY3IwPevpv5MrmPCB";
    /// The certificate's key, PKCS#8 in base64 DER.
    const KEY: &str = "\
        MIGHAgEAMBMGByqGSM49AgEGCCqGSM49AwEHBG0wawIBAQQgpFwcUoHm4lzWLPVx\
        C7C0/ZybofYrndXbyXvqUs1IbSKhRANCAASTpP8eb6+0DZb840yaDpy8hqyg0nay\
        WQtZrss/WF5aR2CbsNb40NlBB8HSk58AVWQCh6knDInXJkZuua2ESQXv";

    fn der(base64: &str) -> Vec<u8> {
        STANDARD.decode(base64).unwrap()
    }

    fn provider() -> Arc<rustls::crypto::CryptoProvider> {
        Arc::new(rustls::crypto::ring::default_provider())
    }

    /// Client TLS that trusts the test certificate and nothing else.
    fn trusting() -> Arc<ClientConfig> {
        let mut roots = rustls::RootCertStore::empty();
        roots.add(der(CERT).into()).unwrap();
        let config = ClientConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_root_certificates(roots)
            .with_no_client_auth();
        Arc::new(config)
    }

    trait Stream: Read + Write + Send {}
    impl<T: Read + Write + Send> Stream for T {}

    /// The server's side of TLS on `tcp`.
    fn wrap(tcp: TcpStream) -> Box<dyn Stream> {
        let key = rustls::pki_types::PrivatePkcs8KeyDer::from(der(KEY));
        let config = rustls::ServerConfig::builder_with_provider(provider())
            .with_safe_default_protocol_versions()
            .unwrap()
            .with_no_client_auth()
            .with_single_cert(vec![der(CERT).into()], key.into())
            .unwrap();
        let connection = rustls::ServerConnection::new(Arc::new(config)).unwrap();
        Box::new(rustls::StreamOwned::new(connection, tcp))
    }

    /// A byte at a time, so nothing sent after STARTTLS is read as plain.
    fn read_line(stream: &mut dyn Stream) -> Option<String> {
        let mut line = Vec::new();
        let mut byte = [0];
        while stream.read(&mut byte).ok()? == 1 {
            if byte[0] == b'\n' {
                return Some(String::from_utf8_lossy(&line).trim_end().to_owned());
            }
            line.push(byte[0]);
        }
        None
    }

    /// A reply, or nothing once the client has gone, as after a failed
    /// handshake.
    fn say(stream: &mut dyn Stream, reply: &str) {
        let _ = stream
            .write_all(format!("{reply}\r\n").as_bytes())
            .and_then(|()| stream.flush());
    }

    /// What a fake server heard: each command, after "tls " once TLS was on
    /// for it, and the mail as DATA carried it, a line to a line.
    #[derive(Default)]
    struct Heard {
        commands: Vec<String>,
        mail: Vec<String>,
    }

    /// A fake mail server for one conversation, so nothing reaches a real
    /// one: TLS from the first byte when `implicit`, else offered by STARTTLS
    /// when `starttls`. `login` is its answer to AUTH.
    fn server(
        implicit: bool,
        starttls: bool,
        login: &'static str,
    ) -> (u16, std::thread::JoinHandle<Heard>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let worker = std::thread::spawn(move || {
            let (tcp, _) = listener.accept().unwrap();
            tcp.set_read_timeout(Some(std::time::Duration::from_secs(5)))
                .unwrap();
            let mut plain = Some(tcp.try_clone().unwrap());
            let mut secure = implicit;
            let mut stream: Box<dyn Stream> = match implicit {
                true => wrap(tcp),
                false => Box::new(tcp),
            };
            let mut heard = Heard::default();
            say(&mut *stream, "220 test ready");
            while let Some(line) = read_line(&mut *stream) {
                let tag = if secure { "tls " } else { "" };
                heard.commands.push(format!("{tag}{line}"));
                match line.to_ascii_uppercase().as_str() {
                    hello if hello.starts_with("EHLO") => match starttls && !secure {
                        true => say(&mut *stream, "250-test\r\n250-STARTTLS\r\n250 AUTH PLAIN"),
                        false => say(&mut *stream, "250-test\r\n250 AUTH PLAIN"),
                    },
                    "STARTTLS" => {
                        say(&mut *stream, "220 go ahead");
                        stream = wrap(plain.take().unwrap());
                        secure = true;
                    }
                    auth if auth.starts_with("AUTH") => say(&mut *stream, login),
                    "DATA" => {
                        say(&mut *stream, "354 go ahead");
                        while let Some(line) = read_line(&mut *stream).filter(|l| l != ".") {
                            heard.mail.push(line);
                        }
                        say(&mut *stream, "250 queued");
                    }
                    "QUIT" => {
                        say(&mut *stream, "221 bye");
                        break;
                    }
                    _ => say(&mut *stream, "250 ok"),
                }
            }
            heard
        });
        (port, worker)
    }

    fn mailbox(port: u16) -> Email {
        Email {
            enabled: true,
            smtp_host: "127.0.0.1".into(),
            port,
            username: "user_a@example.com".into(),
            password: "app-password".into(),
            from: String::new(),
            to: "reports@example.com".into(),
        }
    }

    /// The text of a mail heard, decoded.
    fn text(mail: &[String]) -> String {
        let blank = mail.iter().position(String::is_empty).unwrap();
        String::from_utf8(der(&mail[blank + 1..].concat())).unwrap()
    }

    #[test]
    fn a_report_reaches_a_test_smtp_server() {
        let (port, worker) = server(false, true, "235 2.7.0 Accepted");
        let config = Config {
            email: mailbox(port),
            ..Default::default()
        };
        let tls = trusting();
        let failed = report_via(
            "http://127.0.0.1:1",
            Some(&tls),
            &config,
            "Nightly \u{2713}",
            "All green.\nNo failures.",
        );
        assert!(failed.is_empty(), "{failed:?}");
        let heard = worker.join().unwrap();
        // The password goes only once TLS is on.
        let login = STANDARD.encode("\0user_a@example.com\0app-password");
        let auth = format!("tls AUTH PLAIN {login}");
        assert_eq!(
            heard.commands,
            [
                "EHLO [127.0.0.1]",
                "STARTTLS",
                "tls EHLO [127.0.0.1]",
                auth.as_str(),
                "tls MAIL FROM:<user_a@example.com>",
                "tls RCPT TO:<reports@example.com>",
                "tls DATA",
                "tls QUIT",
            ]
        );
        let subject = format!(
            "Subject: =?UTF-8?B?{}?=",
            STANDARD.encode("Nightly \u{2713}")
        );
        for header in [
            "From: user_a@example.com",
            "To: reports@example.com",
            subject.as_str(),
            "Content-Type: text/plain; charset=utf-8",
        ] {
            assert!(heard.mail.iter().any(|line| line == header), "{header}");
        }
        assert_eq!(text(&heard.mail), "All green.\r\nNo failures.");

        // On port 465, TLS from the first byte, with no STARTTLS asked for.
        let (port, worker) = server(true, false, "235 ok");
        deliver(&tls, &mailbox(port), true, "Nightly", "ok").unwrap();
        let heard = worker.join().unwrap();
        assert!(heard.commands.iter().all(|line| line.starts_with("tls ")));
        assert!(heard.mail.iter().any(|line| line == "Subject: Nightly"));
        assert_eq!(text(&heard.mail), "ok");
    }

    #[test]
    fn a_failed_login_is_shown_before_saving() {
        let (port, worker) = server(false, true, "535 5.7.8 Username and Password not accepted");
        let refused = check_via(&trusting(), &mailbox(port)).unwrap_err();
        assert_eq!(
            refused,
            "127.0.0.1 refused the login (535 5.7.8 Username and Password not accepted). \
             Most providers want an app password rather than the account's own; Outlook and \
             Microsoft 365 take only OAuth, which Letronna cannot do."
        );
        let heard = worker.join().unwrap();
        assert!(heard.commands.iter().all(|line| !line.contains("MAIL")));

        // A server that offers no TLS never sees the password.
        let (port, worker) = server(false, false, "235 ok");
        let refused = check_via(&trusting(), &mailbox(port)).unwrap_err();
        assert!(refused.contains("does not offer TLS"), "{refused}");
        let heard = worker.join().unwrap();
        assert!(heard.commands.iter().all(|line| !line.contains("AUTH")));

        // Nor does one whose certificate the web's roots do not vouch for.
        let (port, worker) = server(true, false, "235 ok");
        let tls = super::tls().unwrap();
        let refused = deliver(tls, &mailbox(port), true, "x", "y").unwrap_err();
        assert!(
            refused.starts_with("Cannot open a secure connection to 127.0.0.1"),
            "{refused}"
        );
        assert!(worker.join().unwrap().commands.is_empty());

        // And a setting left out is said without a connection at all.
        let no_password = Email {
            password: " ".into(),
            ..mailbox(1)
        };
        assert_eq!(
            check_via(&trusting(), &no_password),
            Err("Fill in an app password from the mail provider.".into())
        );
    }

    #[test]
    fn a_subject_or_address_cannot_add_lines_to_the_mail() {
        assert_eq!(
            header_text("Nightly\r\nBcc: someone@example.com"),
            "Nightly  Bcc: someone@example.com"
        );
        // Long or not ASCII: words of whole characters, each within 75.
        let long = "\u{65e5}\u{672c}".repeat(40);
        let header = header_text(&long);
        let words: Vec<&str> = header.split("\r\n ").collect();
        assert!(words.len() > 1 && words.iter().all(|word| word.len() <= 75));
        let decoded: Vec<u8> = words
            .iter()
            .flat_map(|word| der(&word["=?UTF-8?B?".len()..word.len() - 2]))
            .collect();
        assert_eq!(String::from_utf8(decoded).unwrap(), long);

        let smuggled = Email {
            to: "reports@example.com>\r\nRCPT TO:<other@example.com".into(),
            ..mailbox(1)
        };
        let refused = check_via(&trusting(), &smuggled).unwrap_err();
        assert!(refused.contains("is not an email address"), "{refused}");
    }
}
