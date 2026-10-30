//! The connection to letronna-api that phones reach this computer through
//! wherever they are.
//!
//! One connection out carries every phone's at once, so nothing listens on
//! the network for it. It is a WebSocket to `/v1/phone/computer` on the
//! account API, with the account's token as a bearer token and this
//! computer's public key as `key`, base64url, which is how a phone asks the
//! relay for this computer. A second connection with the same key replaces
//! the first.
//!
//! The relay numbers each phone's connection and says which every frame is
//! from or for, in binary messages either way:
//!
//! - one byte: `0` for a frame, `1` for a phone gone, or from here, sent
//!   away;
//! - the phone's number, four bytes big-endian;
//! - for a frame, the frame, which the relay cannot read (`phone.rs` says
//!   what is in it).
//!
//! A number is not used again on the same connection; the switchboard
//! knows each phone by a number of its own, which keeps that true across
//! connections as well.
//!
//! The relay passes frames only between a phone and a computer signed in to
//! the same account. That keeps strangers off it; what keeps them out of
//! this computer is the handshake. A refused token stops the connection,
//! since trying again cannot help, and signing in again starts it. Anything
//! else that ends it is tried again, soon at first and then once a minute,
//! and every phone on it is gone and connects again.

use super::switchboard::{Input, Out, Timing};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use std::collections::HashSet;
use std::net::TcpStream;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, Sender};
use std::time::{Duration, Instant};
use tungstenite::Message as Frame;
use tungstenite::stream::MaybeTlsStream;

/// A message's first byte: a phone's frame.
const FRAME: u8 = 0;
/// A message's first byte: a phone gone, or sent away.
const GONE: u8 = 1;
/// The bytes before a frame: what it is, and whose.
const HEAD: usize = 5;

/// Why the connection stopped for good, said in Settings.
pub(super) const SIGNED_OUT: &str = "Sign in to Letronna to reach this computer from your phone.";
const TOKEN_REFUSED: &str = "Letronna's servers did not accept this computer's sign-in. Sign in \
again to reach it from your phone.";

/// What the connection is doing, for Settings.
#[derive(Clone, Debug, PartialEq)]
pub enum State {
    /// Opening it, or waiting to try again after one failed.
    Connecting,
    Connected,
    /// Stopped until it is started again; why, in words.
    Refused(String),
}

/// Where the relay is, and the account's token: found again before each
/// try, so a new sign-in counts.
pub(super) struct Dial {
    url: String,
    token: String,
}

pub(super) fn dial() -> Result<Dial, String> {
    let account = crate::auth::account()
        .filter(|account| !account.token.is_empty())
        .ok_or(SIGNED_OUT)?;
    let url = crate::auth::api("/v1/phone/computer");
    let url = match url.split_once("://") {
        Some(("https", rest)) => format!("wss://{rest}"),
        Some(("http", rest)) => format!("ws://{rest}"),
        _ => url,
    };
    Ok(Dial {
        url,
        token: account.token,
    })
}

type Socket = tungstenite::WebSocket<MaybeTlsStream<TcpStream>>;

/// How a connection ended.
enum Ended {
    Stopped,
    Refused(String),
    Failed,
}

/// Hold a connection open until `stop` is set, opening another when one
/// fails, and pass what comes and goes on it to the board through `input`.
pub(super) fn run(
    dial: impl Fn() -> Result<Dial, String>,
    input: Sender<Input>,
    stop: Arc<AtomicBool>,
    timing: Timing,
) {
    let tell = |state| {
        let _ = input.send(Input::State(state));
    };
    let mut wait = timing.retry;
    tell(State::Connecting);
    while !stop.load(Ordering::Relaxed) {
        let began = Instant::now();
        let ended = match dial() {
            Ok(dial) => connection(&dial, &input, &stop, timing),
            Err(why) => Ended::Refused(why),
        };
        match ended {
            Ended::Stopped => return,
            Ended::Refused(why) => return tell(State::Refused(why)),
            Ended::Failed => {}
        }
        if began.elapsed() >= timing.retry_most {
            wait = timing.retry;
        }
        tell(State::Connecting);
        std::thread::sleep(wait);
        wait = (wait * 2).min(timing.retry_most);
    }
}

/// One connection, until it ends. Every phone on it goes with it.
fn connection(dial: &Dial, input: &Sender<Input>, stop: &AtomicBool, timing: Timing) -> Ended {
    let Ok(me) = super::computer() else {
        return Ended::Failed;
    };
    let url = format!("{}?key={}", dial.url, URL_SAFE_NO_PAD.encode(me.public()));
    let mut socket = match connect(&url, &dial.token, timing.quiet) {
        Ok(socket) => socket,
        Err(ended) => return ended,
    };
    let wire = super::switchboard::wire();
    let (out, outs) = std::sync::mpsc::channel();
    let _ = input.send(Input::Opened(wire, out));
    let _ = input.send(Input::State(State::Connected));
    let ended = hold(&mut socket, wire, &outs, input, stop, timing);
    if let Ended::Stopped = ended {
        let _ = socket.close(None);
        let _ = socket.flush();
    }
    let _ = input.send(Input::Closed(wire));
    ended
}

fn hold(
    socket: &mut Socket,
    wire: u64,
    outs: &Receiver<Out>,
    input: &Sender<Input>,
    stop: &AtomicBool,
    timing: Timing,
) -> Ended {
    // The phones on it now, as far as this side knows: while there are
    // any, a read gives way soon, so what the board sends them goes out.
    let mut alive = HashSet::new();
    let mut heard = Instant::now();
    let mut pinged = false;
    let mut waits = timing.quiet;
    loop {
        while let Ok(out) = outs.try_recv() {
            let sent = match out {
                Out::Frame(number, frame) => send(socket, FRAME, number, &frame),
                Out::Gone(number) => {
                    alive.remove(&number);
                    send(socket, GONE, number, &[])
                }
            };
            if sent.is_err() {
                return Ended::Failed;
            }
        }
        if stop.load(Ordering::Relaxed) {
            return Ended::Stopped;
        }
        let wait = match alive.is_empty() {
            true => timing.idle,
            false => timing.busy,
        };
        if wait != waits {
            waits = wait;
            wait_for(socket, wait);
        }
        match socket.read() {
            Ok(Frame::Binary(bytes)) => {
                heard = Instant::now();
                pinged = false;
                // One too short to be either kind, or of a kind from a
                // newer relay, is passed over.
                let Some((head, body)) = bytes.split_first_chunk::<HEAD>() else {
                    continue;
                };
                let [kind, number @ ..] = *head;
                let number = u32::from_be_bytes(number);
                let taken = match kind {
                    FRAME => {
                        alive.insert(number);
                        input.send(Input::Frame {
                            wire,
                            number,
                            frame: body.to_vec(),
                        })
                    }
                    GONE => {
                        alive.remove(&number);
                        input.send(Input::Gone { wire, number })
                    }
                    _ => Ok(()),
                };
                if taken.is_err() {
                    return Ended::Stopped;
                }
            }
            Ok(Frame::Close(_)) => return Ended::Failed,
            Ok(_) => {
                heard = Instant::now();
                pinged = false;
            }
            // tungstenite keeps its place across a timed-out read.
            Err(tungstenite::Error::Io(error)) if quiet(&error) => {
                if heard.elapsed() >= timing.quiet * 2 {
                    return Ended::Failed;
                }
                if heard.elapsed() >= timing.quiet && !pinged {
                    pinged = true;
                    if socket.send(Frame::Ping(Default::default())).is_err() {
                        return Ended::Failed;
                    }
                }
            }
            Err(_) => return Ended::Failed,
        }
    }
}

/// Send the relay a message of `kind` for phone `phone`.
fn send(socket: &mut Socket, kind: u8, phone: u32, body: &[u8]) -> Result<(), Ended> {
    let mut bytes = Vec::with_capacity(HEAD + body.len());
    bytes.push(kind);
    bytes.extend_from_slice(&phone.to_be_bytes());
    bytes.extend_from_slice(body);
    socket.send(Frame::binary(bytes)).map_err(|_| Ended::Failed)
}

/// Open the connection, with the account's token. A token the relay turns
/// away is the one failure trying again cannot help.
fn connect(url: &str, token: &str, quiet: Duration) -> Result<Socket, Ended> {
    use tungstenite::client::IntoClientRequest;
    use tungstenite::handshake::HandshakeError;
    use tungstenite::http::{HeaderValue, header::AUTHORIZATION};
    use tungstenite::protocol::WebSocketConfig;
    let mut request = url.into_client_request().map_err(|_| Ended::Failed)?;
    let bearer = HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| Ended::Refused(TOKEN_REFUSED.into()))?;
    request.headers_mut().insert(AUTHORIZATION, bearer);
    let stream = crate::messaging::reach(url, quiet).ok_or(Ended::Failed)?;
    // Nothing from the relay is longer than a frame and its head, so one
    // that is would only take memory.
    let most = HEAD + super::noise::MOST;
    let config = WebSocketConfig::default()
        .max_message_size(Some(most))
        .max_frame_size(Some(most));
    match tungstenite::client_tls_with_config(request, stream, Some(config), None) {
        Ok((socket, _)) => Ok(socket),
        Err(HandshakeError::Failure(tungstenite::Error::Http(response)))
            if matches!(response.status().as_u16(), 401 | 403) =>
        {
            Err(Ended::Refused(TOKEN_REFUSED.into()))
        }
        Err(_) => Err(Ended::Failed),
    }
}

/// Have each read wait at most `time`.
fn wait_for(socket: &Socket, time: Duration) {
    let stream = match socket.get_ref() {
        MaybeTlsStream::Plain(stream) => stream,
        MaybeTlsStream::Rustls(tls) => tls.get_ref(),
        _ => return,
    };
    let _ = stream.set_read_timeout(Some(time));
}

/// A read that timed out, which Unix says would block and Windows timed
/// out, or one a signal broke off, which a read with a timeout is not
/// started again after: tried again either way.
pub(super) fn quiet(error: &std::io::Error) -> bool {
    use std::io::ErrorKind;
    matches!(
        error.kind(),
        ErrorKind::WouldBlock | ErrorKind::TimedOut | ErrorKind::Interrupted
    )
}

#[cfg(test)]
mod tests {
    use super::super::noise::{Handshake, Keys};
    use super::super::switchboard::tests::{FAST, PATIENCE, arrived, data, done, next, state};
    use super::super::switchboard::{Event, PAIRED, REFUSED, Switchboard};
    use super::super::{
        Caller, Hello, Invite, PAIR, PAIRED as KNOWN, PIECE, PROLOGUE, Tunnel, computer, keep,
        phones,
    };
    use super::*;
    use std::net::TcpListener;

    /// A relay on this machine, and the dial that reaches it.
    fn relay() -> (
        TcpListener,
        impl Fn() -> Result<Dial, String> + Send + 'static,
    ) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("ws://{}/v1/phone/computer", listener.local_addr().unwrap());
        let dial = move || {
            Ok(Dial {
                url: url.clone(),
                token: "account-token".into(),
            })
        };
        (listener, dial)
    }

    fn start(
        dial: impl Fn() -> Result<Dial, String> + Send + 'static,
    ) -> (Switchboard, async_channel::Receiver<Event>) {
        let (sender, events) = async_channel::unbounded();
        let board = Switchboard::start_with("Desk".into(), sender, dial, None, FAST).unwrap();
        (board, events)
    }

    type Served = tungstenite::WebSocket<TcpStream>;

    /// This computer's connection, as the relay takes it: what it asked for
    /// is checked, and `status` other than 101 turns it away. The error
    /// response is tungstenite's own, however large.
    #[allow(clippy::result_large_err)]
    fn accept(listener: &TcpListener, status: u16) -> Option<Served> {
        use tungstenite::handshake::server::{ErrorResponse, Request, Response};
        let (stream, _) = listener.accept().unwrap();
        stream.set_read_timeout(Some(PATIENCE)).unwrap();
        let key = URL_SAFE_NO_PAD.encode(computer().unwrap().public());
        let check = move |request: &Request, response: Response| {
            assert_eq!(request.uri().path(), "/v1/phone/computer");
            assert_eq!(request.uri().query(), Some(format!("key={key}").as_str()));
            assert_eq!(request.headers()["authorization"], "Bearer account-token");
            if status == 101 {
                return Ok(response);
            }
            let mut refused = ErrorResponse::new(None);
            *refused.status_mut() = tungstenite::http::StatusCode::from_u16(status).unwrap();
            Err(refused)
        };
        tungstenite::accept_hdr(stream, check).ok()
    }

    /// The next binary message from this computer: its kind, phone and body.
    fn heard(served: &mut Served) -> (u8, u32, Vec<u8>) {
        loop {
            match served.read() {
                Ok(Frame::Binary(bytes)) => {
                    let number: [u8; 4] = bytes[1..HEAD].try_into().unwrap();
                    return (bytes[0], u32::from_be_bytes(number), bytes[HEAD..].to_vec());
                }
                Ok(_) => {}
                Err(tungstenite::Error::Io(error)) if quiet(&error) => {}
                Err(error) => panic!("{error}"),
            }
        }
    }

    fn pass(served: &mut Served, kind: u8, phone: u32, body: &[u8]) {
        let mut bytes = vec![kind];
        bytes.extend_from_slice(&phone.to_be_bytes());
        bytes.extend_from_slice(body);
        served.send(Frame::binary(bytes)).unwrap();
    }

    /// Wait until this computer has dealt with everything passed to it so
    /// far: a first frame that is no handshake is sent away, and the board
    /// deals with what it is sent in order.
    fn settle(served: &mut Served) {
        pass(served, FRAME, 999, b"no handshake");
        assert_eq!(heard(served), (GONE, 999, Vec::new()));
    }

    /// A phone on the relay: its key, and its tunnel once it has one.
    struct Phone {
        keys: Keys,
        number: u32,
        tunnel: Option<Tunnel>,
    }

    impl Phone {
        fn new(number: u32) -> Self {
            Phone {
                keys: Keys::random().unwrap(),
                number,
                tunnel: None,
            }
        }

        fn key(&self) -> String {
            URL_SAFE_NO_PAD.encode(self.keys.public())
        }

        /// Its first frame: pairing with `secret`, or as a phone already
        /// paired; and the handshake to read the answer with.
        fn first(&self, secret: Option<[u8; 32]>) -> (Handshake, Vec<u8>) {
            let computer = computer().unwrap().public();
            let mut handshake = Handshake::initiator(
                PROLOGUE,
                self.keys.clone(),
                computer,
                secret,
                Keys::random().unwrap(),
            );
            let mut first = vec![if secret.is_some() { PAIR } else { KNOWN }];
            first.extend(handshake.write(br#"{"name": "Pocket"}"#).unwrap());
            (handshake, first)
        }

        /// Connect, and read this computer's answer: what it says of
        /// itself.
        fn call(&mut self, served: &mut Served, secret: Option<[u8; 32]>) -> Hello {
            let (mut handshake, first) = self.first(secret);
            pass(served, FRAME, self.number, &first);
            let (kind, phone, reply) = heard(served);
            assert_eq!((kind, phone), (FRAME, self.number));
            let hello: Hello = serde_json::from_slice(&handshake.read(&reply).unwrap()).unwrap();
            self.tunnel = Some(Tunnel::new(handshake.finish().unwrap()));
            hello
        }

        fn say(&mut self, served: &mut Served, message: &[u8]) {
            for frame in self.tunnel.as_mut().unwrap().seal(message).unwrap() {
                pass(served, FRAME, self.number, &frame);
            }
        }

        /// The next whole message this computer sends it.
        fn hear(&mut self, served: &mut Served) -> Vec<u8> {
            loop {
                let (kind, phone, body) = heard(served);
                assert_eq!((kind, phone), (FRAME, self.number));
                if let Some(message) = self.tunnel.as_mut().unwrap().open(&body).unwrap() {
                    return message;
                }
            }
        }
    }

    #[test]
    fn a_paired_phone_talks_to_this_computer_through_the_relay() {
        let (_held, _dir) = data("relay-paired");
        let mut phone = Phone::new(7);
        keep("Old name", &phone.key()).unwrap();
        let (listener, dial) = relay();
        let (board, events) = start(dial);
        let mut served = accept(&listener, 101).unwrap();
        assert_eq!(state(&events), State::Connecting);
        assert_eq!(state(&events), State::Connected);

        let hello = phone.call(&mut served, None);
        // Not answering directly, it gives no address for that.
        assert_eq!((hello.name.as_str(), hello.direct.len()), ("Desk", 0));
        // It is there once it speaks, which a paired phone does first.
        let long = vec![b'x'; PIECE * 2 + 3];
        phone.say(&mut served, &long);
        let Event::Arrived {
            phone: number,
            caller: Caller::Paired(known),
        } = next(&events)
        else {
            panic!("a paired phone arrives as paired");
        };
        assert_eq!((known.name.as_str(), known.key), ("Pocket", phone.key()));
        assert!(
            matches!(next(&events), Event::Message { phone, message } if phone == number && message == long)
        );
        // Listed by the name it gave, as of now.
        assert_eq!(phones()[0].name, "Pocket");

        board.send(number, br#"{"answer": 1}"#.to_vec());
        assert_eq!(phone.hear(&mut served), br#"{"answer": 1}"#);

        pass(&mut served, GONE, 7, &[]);
        assert!(matches!(next(&events), Event::Gone { phone, why: None } if phone == number));
        // Gone, so what is sent to it now goes nowhere.
        board.send(number, b"late".to_vec());
        settle(&mut served);
        done(board);
    }

    #[test]
    fn an_old_first_message_sent_again_is_answered_but_never_arrives() {
        let (_held, _dir) = data("relay-replay");
        let mut phone = Phone::new(7);
        keep("Pocket", &phone.key()).unwrap();
        let (listener, dial) = relay();
        let (board, events) = start(dial);
        let mut served = accept(&listener, 101).unwrap();

        let (mut handshake, first) = phone.first(None);
        pass(&mut served, FRAME, 7, &first);
        let (_, _, reply) = heard(&mut served);
        handshake.read(&reply).unwrap();
        phone.tunnel = Some(Tunnel::new(handshake.finish().unwrap()));
        // Whoever saw it pass sends it again: it is answered, as it cannot
        // be told from a new one, but nothing can come of the answer.
        pass(&mut served, FRAME, 8, &first);
        assert_eq!(heard(&mut served).0, FRAME);
        pass(&mut served, GONE, 8, &[]);

        phone.say(&mut served, b"hello");
        let number = arrived(&events);
        assert!(matches!(next(&events), Event::Message { phone, .. } if phone == number));
        pass(&mut served, GONE, 7, &[]);
        // Neither the copy's coming nor its going was told.
        assert!(matches!(next(&events), Event::Gone { phone, .. } if phone == number));
        done(board);
    }

    #[test]
    fn a_phone_pairs_once_the_user_says_yes_and_is_heard_only_after() {
        let (_held, _dir) = data("relay-pair");
        let invite = Invite::new().unwrap();
        let secret = invite.secret;
        let (listener, dial) = relay();
        let (board, events) = start(dial);
        let mut served = accept(&listener, 101).unwrap();
        board.invite(Some(invite));

        let mut phone = Phone::new(3);
        phone.call(&mut served, Some(secret));
        let Event::Arrived {
            phone: number,
            caller: Caller::New { name, key, code },
        } = next(&events)
        else {
            panic!("a pairing phone arrives as new, before it says anything");
        };
        assert_eq!((name.as_str(), key), ("Pocket", phone.key()));
        assert_eq!(code.len(), 6);
        assert!(phones().is_empty());

        // Before the yes, what it says is not read, and nothing is sent it.
        phone.say(&mut served, b"too soon");
        board.send(number, b"nor is it sent anything".to_vec());
        settle(&mut served);
        board.keep(number);
        assert_eq!(phone.hear(&mut served), PAIRED.as_bytes());
        assert_eq!(phones()[0].key, phone.key());
        phone.say(&mut served, b"now");
        assert!(matches!(next(&events), Event::Message { message, .. } if message == b"now"));

        // The code is used up: another phone with it is sent away.
        let (_, first) = Phone::new(4).first(Some(secret));
        pass(&mut served, FRAME, 4, &first);
        assert_eq!(heard(&mut served), (GONE, 4, Vec::new()));
        done(board);
    }

    #[test]
    fn a_phone_the_user_says_no_to_is_told_and_sent_away() {
        let (_held, _dir) = data("relay-refuse");
        let invite = Invite::new().unwrap();
        let secret = invite.secret;
        let (listener, dial) = relay();
        let (board, events) = start(dial);
        let mut served = accept(&listener, 101).unwrap();
        board.invite(Some(invite));

        let mut phone = Phone::new(5);
        phone.call(&mut served, Some(secret));
        let number = arrived(&events);
        board.refuse(number);
        assert_eq!(phone.hear(&mut served), REFUSED.as_bytes());
        assert_eq!(heard(&mut served), (GONE, 5, Vec::new()));
        assert!(matches!(next(&events), Event::Gone { phone, why: None } if phone == number));
        assert!(phones().is_empty());
        done(board);
    }

    #[test]
    fn a_phone_not_paired_is_sent_away_and_nothing_arrives() {
        let (_held, _dir) = data("relay-stranger");
        let mut known = Phone::new(1);
        keep("Pocket", &known.key()).unwrap();
        let (listener, dial) = relay();
        let (board, events) = start(dial);
        let mut served = accept(&listener, 101).unwrap();

        let (_, first) = Phone::new(9).first(None);
        pass(&mut served, FRAME, 9, &first);
        assert_eq!(heard(&mut served), (GONE, 9, Vec::new()));
        // A message of a kind from a newer relay is passed over, and the
        // connection carries on.
        pass(&mut served, 7, 9, b"something new");
        known.call(&mut served, None);
        known.say(&mut served, b"{}");
        assert!(
            matches!(next(&events), Event::Arrived { caller: Caller::Paired(phone), .. } if phone.key == known.key())
        );
        done(board);
    }

    #[test]
    fn a_phone_whose_frame_fails_its_check_is_sent_away() {
        let (_held, _dir) = data("relay-changed");
        let mut phone = Phone::new(6);
        keep("Pocket", &phone.key()).unwrap();
        let (listener, dial) = relay();
        let (board, events) = start(dial);
        let mut served = accept(&listener, 101).unwrap();
        phone.call(&mut served, None);
        phone.say(&mut served, b"{}");
        assert!(matches!(next(&events), Event::Arrived { .. }));
        assert!(matches!(next(&events), Event::Message { .. }));

        let tunnel = phone.tunnel.as_mut().unwrap();
        let mut frame = tunnel.seal(b"hello").unwrap().remove(0);
        frame[0] ^= 1;
        pass(&mut served, FRAME, 6, &frame);
        assert_eq!(heard(&mut served), (GONE, 6, Vec::new()));
        assert!(matches!(next(&events), Event::Gone { why: Some(_), .. }));
        done(board);
    }

    #[test]
    fn a_refused_sign_in_stops_the_connection() {
        let (_held, _dir) = data("relay-refused");
        let (listener, dial) = relay();
        let (board, events) = start(dial);
        assert!(accept(&listener, 401).is_none());
        assert_eq!(state(&events), State::Connecting);
        assert_eq!(state(&events), State::Refused(TOKEN_REFUSED.into()));
        done(board);
    }

    #[test]
    fn signed_out_it_does_not_try() {
        let (_held, _dir) = data("relay-signed-out");
        let (board, events) = start(|| Err(SIGNED_OUT.to_owned()));
        assert_eq!(state(&events), State::Connecting);
        assert_eq!(state(&events), State::Refused(SIGNED_OUT.into()));
        done(board);
    }

    #[test]
    fn a_dropped_connection_is_opened_again_and_its_phones_are_gone() {
        let (_held, _dir) = data("relay-again");
        let mut phone = Phone::new(2);
        keep("Pocket", &phone.key()).unwrap();
        let (listener, dial) = relay();
        let (board, events) = start(dial);
        let mut served = accept(&listener, 101).unwrap();
        phone.call(&mut served, None);
        phone.say(&mut served, b"{}");
        let before = arrived(&events);
        assert!(matches!(next(&events), Event::Message { .. }));

        drop(served);
        assert!(matches!(next(&events), Event::Gone { phone, why: None } if phone == before));
        assert_eq!(state(&events), State::Connecting);
        let mut served = accept(&listener, 101).unwrap();
        assert_eq!(state(&events), State::Connected);
        assert_eq!(phone.call(&mut served, None).name, "Desk");
        phone.say(&mut served, b"{}");
        // The relay gave it the same number, but it is a new arrival, and
        // what was meant for the old one does not reach it.
        let after = arrived(&events);
        assert_ne!(after, before);
        board.send(before, b"stale".to_vec());
        board.send(after, b"fresh".to_vec());
        assert_eq!(phone.hear(&mut served), b"fresh");
        done(board);
    }
}
