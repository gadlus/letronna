//! Phones reaching this computer directly, over Tailscale, when it is asked
//! to answer them that way.
//!
//! When this computer is on a tailnet, a phone on the same one connects to
//! it without the relay: a WebSocket to `ws://<address>:<port>/phone`, the
//! address being this computer's on the tailnet. Each connection is one
//! phone's, and carries its frames as `phone.rs` says, one binary message
//! each with nothing before it. When the phone goes, or is sent away, the
//! connection is closed. A phone learns where to connect from the QR code
//! it pairs with (`d`), so it can pair this way too, and from this
//! computer's answer to every handshake, which lists it as `direct`, over
//! the relay or directly; it tries it first the next time.
//!
//! Only the tailnet address is listened on, so the port is not open on the
//! local network or the internet, and nothing but a device on the user's
//! tailnet reaches it. It is ws, not wss: the tailnet is WireGuard, and the
//! frames are the same ones the relay carries, encrypted end to end, so a
//! device that reaches the port gets no further than the relay would let
//! it: a phone that is not paired is sent away after its first frame.
//!
//! The address is the one the system would send from to reach Tailscale's
//! resolver, 100.100.100.100, when that is in the range Tailscale gives
//! addresses from. It is looked for again every so often, so turning
//! Tailscale on or off is noticed while Letronna runs.

use super::switchboard::{Input, Out, Timing};
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::Sender;
use std::time::{Duration, Instant};
use tungstenite::Message as Frame;
use tungstenite::protocol::CloseFrame;
use tungstenite::protocol::frame::coding::CloseCode;

/// The port listened on unless Settings say another.
pub const PORT: u16 = 47821;
/// The path a phone asks for; any is answered.
const PATH: &str = "/phone";
/// Connections held at once: a bound on what anything on the tailnet can
/// make this computer hold.
const OPEN: usize = 16;
/// How long a connection has to bring its first frame.
const FIRST: Duration = Duration::from_secs(10);
/// How often the listener looks for a phone connecting.
const POLL: Duration = Duration::from_millis(100);
/// Tailscale's resolver, at the same address in every tailnet.
const RESOLVER: Ipv4Addr = Ipv4Addr::new(100, 100, 100, 100);

/// Whether phones can reach this computer directly, for Settings.
#[derive(Clone, Debug, PartialEq)]
pub enum Direct {
    /// Phones on the tailnet can connect, at this address and port.
    Answering(String),
    /// This computer is not on a tailnet now: Tailscale is not running on
    /// it, or not signed in.
    NoTailnet,
    /// Its tailnet address could not be listened on; why.
    Failed(String),
}

/// Where to listen: the port, and what finds the tailnet address.
pub(super) struct Listen {
    pub port: u16,
    pub find: fn() -> Option<IpAddr>,
}

/// What a phone is told to connect to, for an address and port.
pub(super) fn url(at: &str) -> String {
    format!("ws://{at}{PATH}")
}

/// This computer's address on a tailnet, if it is on one now.
pub(super) fn tailnet() -> Option<IpAddr> {
    let from = |to: SocketAddr| {
        let socket = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok()?;
        // Nothing is sent: connecting a UDP socket only picks its route.
        socket.connect(to).ok()?;
        match socket.local_addr().ok()?.ip() {
            IpAddr::V4(ip) => Some(ip),
            IpAddr::V6(_) => None,
        }
    };
    let ip = from((RESOLVER, 53).into()).filter(|&ip| on_tailnet(ip))?;
    // A carrier that hands out addresses from the same range would give
    // one that is also the way to the internet. So does an exit node,
    // which is Tailscale's, and has its resolver there to answer.
    let internet = from((Ipv4Addr::new(1, 1, 1, 1), 53).into());
    (internet != Some(ip) || resolver_answers()).then_some(IpAddr::V4(ip))
}

/// Whether `ip` is in 100.64.0.0/10, the range Tailscale's addresses are
/// from.
fn on_tailnet(ip: Ipv4Addr) -> bool {
    let [first, second, ..] = ip.octets();
    first == 100 && (64..128).contains(&second)
}

/// Whether Tailscale's resolver answers a question: what the root's name
/// servers are.
fn resolver_answers() -> bool {
    let Ok(socket) = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)) else {
        return false;
    };
    let _ = socket.set_read_timeout(Some(Duration::from_secs(1)));
    let question = [0x4c, 0x54, 1, 0, 0, 1, 0, 0, 0, 0, 0, 0, 0, 0, 2, 0, 1];
    if socket.send_to(&question, (RESOLVER, 53)).is_err() {
        return false;
    }
    let mut reply = [0u8; 512];
    matches!(socket.recv_from(&mut reply), Ok((n, _)) if n >= 2 && reply[..2] == question[..2])
}

/// Listen on the tailnet address while there is one, until `stop` is set,
/// and pass what each connection brings to the board through `input`.
pub(super) fn run(listen: Listen, input: Sender<Input>, stop: Arc<AtomicBool>, timing: Timing) {
    let open = Arc::new(AtomicUsize::new(0));
    let mut bound: Option<(IpAddr, TcpListener)> = None;
    let mut said: Option<Direct> = None;
    let mut looked: Option<Instant> = None;
    while !stop.load(Ordering::Relaxed) {
        if looked.is_none_or(|at| at.elapsed() >= timing.look) {
            looked = Some(Instant::now());
            let found = (listen.find)();
            if found.is_none() || bound.as_ref().map(|(ip, _)| *ip) != found {
                bound = None;
                let now = match found {
                    None => Direct::NoTailnet,
                    Some(ip) => match take(ip, listen.port) {
                        Ok((listener, at)) => {
                            bound = Some((ip, listener));
                            Direct::Answering(at.to_string())
                        }
                        Err(why) => Direct::Failed(why),
                    },
                };
                if said.as_ref() != Some(&now) {
                    if input.send(Input::Direct(now.clone())).is_err() {
                        return;
                    }
                    said = Some(now);
                }
            }
        }
        let Some((_, listener)) = &bound else {
            std::thread::sleep(POLL);
            continue;
        };
        let Ok((stream, _)) = listener.accept() else {
            std::thread::sleep(POLL);
            continue;
        };
        if open.load(Ordering::Relaxed) >= OPEN {
            continue;
        }
        let held = Held::new(&open);
        let (input, stop) = (input.clone(), stop.clone());
        let _ = std::thread::Builder::new()
            .name("phone-direct-line".into())
            .spawn(move || {
                let _held = held;
                serve(stream, &input, &stop, timing);
            });
    }
}

/// Listen on `ip`, at `port` or, when another program has that, any port:
/// phones are told which each time they connect.
fn take(ip: IpAddr, port: u16) -> Result<(TcpListener, SocketAddr), String> {
    let failed = |e: std::io::Error| format!("Could not listen on the tailnet address {ip}: {e}");
    let listener = TcpListener::bind((ip, port))
        .or_else(|_| TcpListener::bind((ip, 0)))
        .map_err(failed)?;
    listener.set_nonblocking(true).map_err(failed)?;
    let at = listener.local_addr().map_err(failed)?;
    Ok((listener, at))
}

/// One of the connections held, counted while it is.
struct Held(Arc<AtomicUsize>);

impl Held {
    fn new(open: &Arc<AtomicUsize>) -> Self {
        open.fetch_add(1, Ordering::Relaxed);
        Held(open.clone())
    }
}

impl Drop for Held {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::Relaxed);
    }
}

/// One phone's connection, until it ends.
fn serve(stream: TcpStream, input: &Sender<Input>, stop: &AtomicBool, timing: Timing) {
    use tungstenite::protocol::WebSocketConfig;
    // Taken from a listener that does not wait, which some systems pass on.
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_nodelay(true);
    let _ = stream.set_read_timeout(Some(FIRST));
    // Nothing a phone sends is longer than a frame.
    let most = super::noise::MOST;
    let config = WebSocketConfig::default()
        .max_message_size(Some(most))
        .max_frame_size(Some(most));
    let Ok(mut socket) = tungstenite::accept_with_config(stream, Some(config)) else {
        return;
    };
    let _ = socket.get_ref().set_read_timeout(Some(timing.busy));
    let wire = super::switchboard::wire();
    let (out, outs) = std::sync::mpsc::channel();
    if input.send(Input::Opened(wire, out)).is_err() {
        return;
    }
    let opened = Instant::now();
    let mut spoke = false;
    let mut heard = Instant::now();
    let mut pinged = false;
    'held: loop {
        while let Ok(out) = outs.try_recv() {
            match out {
                Out::Frame(_, frame) => {
                    if socket.send(Frame::binary(frame)).is_err() {
                        break 'held;
                    }
                }
                Out::Gone(_) => {
                    let _ = socket.close(Some(CloseFrame {
                        code: CloseCode::Normal,
                        reason: Default::default(),
                    }));
                    let _ = socket.flush();
                    break 'held;
                }
            }
        }
        if stop.load(Ordering::Relaxed) || (!spoke && opened.elapsed() >= FIRST) {
            let _ = socket.close(None);
            let _ = socket.flush();
            break;
        }
        match socket.read() {
            Ok(Frame::Binary(bytes)) => {
                (spoke, heard, pinged) = (true, Instant::now(), false);
                let frame = Input::Frame {
                    wire,
                    number: 0,
                    frame: bytes.to_vec(),
                };
                if input.send(frame).is_err() {
                    break;
                }
            }
            Ok(Frame::Close(_)) => {
                // Answered as it goes.
                let _ = socket.flush();
                break;
            }
            Ok(_) => (heard, pinged) = (Instant::now(), false),
            Err(tungstenite::Error::Io(error)) if super::relay::quiet(&error) => {
                if heard.elapsed() >= timing.quiet * 2 {
                    break;
                }
                if heard.elapsed() >= timing.quiet && !pinged {
                    pinged = true;
                    if socket.send(Frame::Ping(Default::default())).is_err() {
                        break;
                    }
                }
            }
            Err(_) => break,
        }
    }
    let _ = input.send(Input::Closed(wire));
}

#[cfg(test)]
mod tests {
    use super::super::noise::{Handshake, Keys};
    use super::super::relay::{SIGNED_OUT, quiet};
    use super::super::switchboard::tests::{FAST, PATIENCE, arrived, data, direct, done, next};
    use super::super::switchboard::{Event, PAIRED as TOLD_PAIRED, Switchboard};
    use super::super::{
        Caller, Hello, Invite, PAIR, PAIRED, PROLOGUE, Tunnel, computer, keep, phones,
    };
    use super::*;
    use base64::Engine;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use tungstenite::stream::MaybeTlsStream;

    type Client = tungstenite::WebSocket<MaybeTlsStream<TcpStream>>;

    /// A board answering directly on this machine, as though it were the
    /// tailnet, and not reaching the relay: where it answers.
    fn start(name: &str) -> (Switchboard, async_channel::Receiver<Event>, String) {
        let (sender, events) = async_channel::unbounded();
        let listen = Listen {
            port: 0,
            find: || Some(IpAddr::V4(Ipv4Addr::LOCALHOST)),
        };
        let signed_out = || Err(SIGNED_OUT.to_owned());
        let board =
            Switchboard::start_with(name.into(), sender, signed_out, Some(listen), FAST).unwrap();
        let Direct::Answering(at) = direct(&events) else {
            panic!("it answers on the address found");
        };
        (board, events, at)
    }

    fn connect(at: &str) -> Client {
        let (client, _) = tungstenite::connect(url(at)).unwrap();
        if let MaybeTlsStream::Plain(stream) = client.get_ref() {
            stream.set_read_timeout(Some(PATIENCE)).unwrap();
        }
        client
    }

    /// A phone's first frame, sent, as a paired phone's or pairing with
    /// `secret`; this computer's answer, read, and the tunnel after it.
    fn call(client: &mut Client, phone: &Keys, secret: Option<[u8; 32]>) -> (Hello, Tunnel) {
        let mut handshake = Handshake::initiator(
            PROLOGUE,
            phone.clone(),
            computer().unwrap().public(),
            secret,
            Keys::random().unwrap(),
        );
        let mut first = vec![if secret.is_some() { PAIR } else { PAIRED }];
        first.extend(handshake.write(br#"{"name": "Pocket"}"#).unwrap());
        client.send(Frame::binary(first)).unwrap();
        let reply = binary(client).expect("an answer");
        let hello: Hello = serde_json::from_slice(&handshake.read(&reply).unwrap()).unwrap();
        (hello, Tunnel::new(handshake.finish().unwrap()))
    }

    /// The next binary message, or none once the connection is closed:
    /// with the close's code.
    fn binary(client: &mut Client) -> Result<Vec<u8>, Option<CloseCode>> {
        loop {
            match client.read() {
                Ok(Frame::Binary(bytes)) => return Ok(bytes.to_vec()),
                Ok(Frame::Close(close)) => return Err(close.map(|close| close.code)),
                Ok(_) => {}
                Err(tungstenite::Error::Io(error)) if quiet(&error) => {}
                Err(_) => return Err(None),
            }
        }
    }

    #[test]
    fn only_tailscales_range_is_a_tailnet() {
        for (ip, on) in [
            ([100, 64, 0, 1], true),
            ([100, 101, 102, 103], true),
            ([100, 127, 255, 255], true),
            ([100, 63, 255, 255], false),
            ([100, 128, 0, 0], false),
            ([192, 168, 1, 2], false),
            ([10, 0, 0, 1], false),
        ] {
            assert_eq!(on_tailnet(Ipv4Addr::from(ip)), on, "{ip:?}");
        }
    }

    #[test]
    fn a_paired_phone_talks_to_this_computer_directly() {
        let (_held, _dir) = data("direct-paired");
        let phone = Keys::random().unwrap();
        keep("Pocket", &URL_SAFE_NO_PAD.encode(phone.public())).unwrap();
        let (board, events, at) = start("Desk");

        let mut client = connect(&at);
        let (hello, mut tunnel) = call(&mut client, &phone, None);
        // It is told where it connected, to keep for next time.
        assert_eq!(hello.name, "Desk");
        assert_eq!(hello.direct, vec![format!("ws://{at}/phone")]);
        for frame in tunnel.seal(b"{}").unwrap() {
            client.send(Frame::binary(frame)).unwrap();
        }
        let Event::Arrived {
            phone: number,
            caller: Caller::Paired(known),
        } = next(&events)
        else {
            panic!("a paired phone arrives as paired");
        };
        assert_eq!(known.key, URL_SAFE_NO_PAD.encode(phone.public()));
        assert!(matches!(next(&events), Event::Message { message, .. } if message == b"{}"));

        board.send(number, b"answer".to_vec());
        let frame = binary(&mut client).unwrap();
        assert_eq!(tunnel.open(&frame).unwrap().unwrap(), b"answer");

        // Hung up on, its connection is closed, and nothing else.
        board.hang_up(number);
        assert_eq!(binary(&mut client), Err(Some(CloseCode::Normal)));
        assert!(matches!(next(&events), Event::Gone { phone, why: None } if phone == number));
        done(board);
    }

    #[test]
    fn a_phone_that_leaves_is_gone() {
        let (_held, _dir) = data("direct-leaves");
        let phone = Keys::random().unwrap();
        keep("Pocket", &URL_SAFE_NO_PAD.encode(phone.public())).unwrap();
        let (board, events, at) = start("Desk");
        let mut client = connect(&at);
        let (_, mut tunnel) = call(&mut client, &phone, None);
        for frame in tunnel.seal(b"{}").unwrap() {
            client.send(Frame::binary(frame)).unwrap();
        }
        let number = arrived(&events);
        assert!(matches!(next(&events), Event::Message { .. }));
        client.close(None).unwrap();
        let _ = binary(&mut client);
        assert!(matches!(next(&events), Event::Gone { phone, .. } if phone == number));
        done(board);
    }

    #[test]
    fn a_phone_pairs_straight_over_the_tailnet() {
        let (_held, _dir) = data("direct-pair");
        let (board, events, at) = start("Desk");
        let invite = Invite::new().unwrap().reached_at(&at);
        let secret = invite.secret;
        assert!(invite.link().ends_with(&format!("&d={at}")));
        board.invite(Some(invite));

        let phone = Keys::random().unwrap();
        let mut client = connect(&at);
        let (_, mut tunnel) = call(&mut client, &phone, Some(secret));
        let Event::Arrived {
            phone: number,
            caller: Caller::New { key, .. },
        } = next(&events)
        else {
            panic!("a pairing phone arrives as new, before it says anything");
        };
        assert_eq!(key, URL_SAFE_NO_PAD.encode(phone.public()));
        board.keep(number);
        let frame = binary(&mut client).unwrap();
        assert_eq!(
            tunnel.open(&frame).unwrap().unwrap(),
            TOLD_PAIRED.as_bytes()
        );
        assert_eq!(phones()[0].key, key);
        done(board);
    }

    #[test]
    fn a_phone_not_paired_is_sent_away() {
        let (_held, _dir) = data("direct-stranger");
        let (board, events, at) = start("Desk");
        let mut client = connect(&at);
        let mut handshake = Handshake::initiator(
            PROLOGUE,
            Keys::random().unwrap(),
            computer().unwrap().public(),
            None,
            Keys::random().unwrap(),
        );
        let mut first = vec![PAIRED];
        first.extend(handshake.write(b"{}").unwrap());
        client.send(Frame::binary(first)).unwrap();
        assert_eq!(binary(&mut client), Err(Some(CloseCode::Normal)));
        // A phone that is paired still gets in after it.
        let phone = Keys::random().unwrap();
        keep("Pocket", &URL_SAFE_NO_PAD.encode(phone.public())).unwrap();
        let mut client = connect(&at);
        let (_, mut tunnel) = call(&mut client, &phone, None);
        for frame in tunnel.seal(b"{}").unwrap() {
            client.send(Frame::binary(frame)).unwrap();
        }
        assert!(
            matches!(next(&events), Event::Arrived { caller: Caller::Paired(known), .. } if known.key == URL_SAFE_NO_PAD.encode(phone.public()))
        );
        done(board);
    }
}
