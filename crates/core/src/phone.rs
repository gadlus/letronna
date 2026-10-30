//! The phone app's remote for this computer: which phones may use it, how
//! one is paired, and the channel each one talks through.
//!
//! The phone and this computer each keep a connection out to letronna-api,
//! which passes frames between them, so for that nothing here listens on
//! the network. The relay only passes them on. When the user turns it on,
//! a phone on the same tailnet can connect straight to this computer
//! instead (`direct`), with the same frames. What they carry is encrypted end
//! to end with keys it never holds, and this computer trusts a phone for
//! the key it pinned when the two were paired, not for the account they are
//! both signed in to. Whoever runs the relay or gets into it, or into the
//! account, can keep the two apart and see when and how much they say, but
//! cannot read it, change it, or speak as either.
//!
//! A phone is paired where the user can see both screens. The window shows
//! a QR code of `letronna://pair?v=1&k=<key>&s=<secret>`: this computer's
//! public key, and a secret good for one phone and `LIFE`, both base64url
//! without padding; and when a phone on the same tailnet can reach it
//! directly, `&d=<address>:<port>`, which a phone connects to as `direct`
//! says. The phone answers that way or through the relay with a key of its
//! own, in a handshake only the holder of that secret can make, and both
//! screens then show six digits drawn from the handshake: the first eight
//! bytes of its hash, big-endian, modulo a million. The window asks before
//! the phone is kept, so someone who copied the code from over a shoulder
//! and got in first is the phone whose digits are not on the one in the
//! user's hand.
//!
//! On the wire, each frame is one binary WebSocket message, passed whole and
//! in order:
//!
//! 1. The phone's first: one byte, `1` to pair or `2` for a phone already
//!    paired, then Noise's first message. Pairing is
//!    `Noise_IKpsk1_25519_ChaChaPoly_SHA256` with the code's secret as the
//!    pre-shared key; a paired phone's is `Noise_IK_25519_ChaChaPoly_SHA256`.
//!    Both have the prologue `letronna phone 1`, and the payload
//!    `{"name": "<the phone's name>"}`.
//! 2. This computer's answer: Noise's second message, whose payload is
//!    `{"name": "<this computer's name>"}`, with
//!    `"direct": ["ws://<address>:<port>/phone"]` when a phone can reach it
//!    over Tailscale too.
//! 3. From then on, either way, messages in pieces, each piece one Noise
//!    transport message. A piece's first byte is `1` while more of its
//!    message follows and `0` on the last.
//!
//! The messages are JSON-RPC 2.0, as the Agent Client Protocol's are. A
//! phone that paired is told the user's answer first: the notification
//! `_letronna/paired`, or `_letronna/refused` before it is sent away. Until
//! the yes, nothing it sends is read. A phone already paired speaks first,
//! as an ACP client does with `initialize`: its first frame alone could be
//! an old one sent again, so it is not taken to be there until it does.
//!
//! A frame that fails a check ends the connection, and the phone connects
//! again with a new handshake. Nothing is retried in place: once a message
//! is lost or refused, the two sides' counts no longer agree. How frames
//! cross the relay is `relay`'s, how they come directly is `direct`'s, and
//! `switchboard` answers them whichever way they came.

pub mod bridge;
pub mod direct;
mod noise;
pub mod relay;
pub mod switchboard;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use noise::{Handshake, Keys, Transport};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::path::PathBuf;
use std::time::{Duration, Instant};

/// Mixed into every handshake first, so one made for anything else, or for
/// another version of this, fails.
const PROLOGUE: &[u8] = b"letronna phone 1";
/// A first frame's opening byte when the phone is pairing.
const PAIR: u8 = 1;
/// A first frame's opening byte when the phone is already paired.
const PAIRED: u8 = 2;
/// How long a QR code can be used: as long as a chat's pairing code.
pub const LIFE: Duration = crate::messaging::pairing::LIFE;
/// What a piece's first byte says.
const MORE: u8 = 1;
const LAST: u8 = 0;
/// The most of a message one piece carries: what a Noise message holds,
/// less the byte that says whether more follows.
const PIECE: usize = noise::MOST - noise::TAG - 1;
/// The most a message put together from pieces may come to. A session's
/// whole history goes over as messages of a step each, so this is far more
/// than one needs, and it keeps a phone gone wrong from filling memory.
const LONGEST: usize = 16 << 20;
/// A phone's name, as it gives it, is cut to this many characters.
const NAME_CHARS: usize = 60;
/// Where the vault keeps this computer's key.
const KEY: &str = "phone.key";

/// A phone paired with this computer.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Phone {
    /// As the phone last gave it.
    pub name: String,
    /// Its public key, base64url: what it is known by.
    pub key: String,
    /// When it was paired, and when it last connected, in Unix seconds.
    pub paired: u64,
    pub seen: u64,
}

/// `phones.json`: the paired phones, and this computer's own key where the
/// system has no vault.
#[derive(Default, Deserialize)]
struct Kept {
    #[serde(default)]
    key: String,
    #[serde(default)]
    phones: Vec<Phone>,
}

pub fn path() -> PathBuf {
    crate::chat::data_dir().join("phones.json")
}

/// Where in `phones.json` the key sits, and its name in the vault.
pub(crate) const SECRETS: &[(&str, &str)] = &[("/key", KEY)];

/// The phones paired with this computer.
pub fn phones() -> Vec<Phone> {
    crate::state::read::<Kept>(&path())
        .map(|kept| kept.phones)
        .unwrap_or_default()
}

/// Keep the phone with key `key`, whose six digits the user has just said
/// match: paired from now on, by `name`.
fn keep(name: &str, key: &str) -> Result<(), String> {
    let now = crate::chat::now();
    change(|phones| {
        phones.retain(|phone| phone.key != key);
        phones.push(Phone {
            name: name.to_owned(),
            key: key.to_owned(),
            paired: now,
            seen: now,
        });
    })
}

/// Unpair the phone with key `key`. One connected now is not cut off by
/// this alone: the host hangs up on it too.
pub fn forget(key: &str) -> Result<(), String> {
    change(|phones| phones.retain(|phone| phone.key != key))
}

/// Note that the phone with key `key` connected, by `name`.
fn seen(key: &str, name: &str) -> Result<(), String> {
    let now = crate::chat::now();
    change(|phones| {
        if let Some(phone) = phones.iter_mut().find(|phone| phone.key == key) {
            phone.seen = now;
            if !name.is_empty() {
                phone.name = name.to_owned();
            }
        }
    })
}

/// Change the list of phones, under the file's lock and from what it holds
/// now, leaving this computer's key as it is.
fn change(edit: impl FnOnce(&mut Vec<Phone>)) -> Result<(), String> {
    crate::state::update::<Kept>(&path(), |disk| {
        if !disk.is_object() {
            *disk = Value::Object(Default::default());
        }
        let mut phones: Vec<Phone> =
            serde_json::from_value(disk["phones"].take()).unwrap_or_default();
        edit(&mut phones);
        disk["phones"] = serde_json::to_value(phones).unwrap_or_default();
    })
}

/// This computer's key, made the first time a phone is paired. A copy in
/// the file wins, as it does for every secret. A vault that will not give
/// it is an error, never a reason to make another, which would unpair
/// every phone.
fn computer() -> Result<Keys, String> {
    let in_file = crate::state::read::<Kept>(&path())?.key;
    let text = match in_file.is_empty() {
        true => crate::vault::get(KEY)?.unwrap_or_default(),
        false => in_file,
    };
    if !text.is_empty() {
        return decode(&text)
            .map(Keys::from_secret)
            .ok_or_else(|| "This computer's key for phones is damaged.".to_owned());
    }
    let made = Keys::random()?;
    keep_key(&URL_SAFE_NO_PAD.encode(made.secret()))?;
    Ok(made)
}

/// Keep this computer's key in the vault, or in `phones.json` where there
/// is none, as a wallet's key is kept.
fn keep_key(secret: &str) -> Result<(), String> {
    let mut refused = None;
    crate::state::update::<Kept>(&path(), |disk| {
        if !disk.is_object() {
            *disk = Value::Object(Default::default());
        }
        let in_file = disk["key"].as_str().unwrap_or_default().to_owned();
        match crate::vault::save(KEY, secret, &in_file) {
            Ok(kept) => disk["key"] = Value::from(kept),
            Err(why) => refused = Some(why),
        }
    })?;
    refused.map_or(Ok(()), |why| {
        Err(format!(
            "Could not keep this computer's key for phones: {why}."
        ))
    })
}

/// This computer's key, to be kept out of what tools return.
pub(crate) fn secrets() -> Vec<String> {
    let in_file = crate::state::read::<Kept>(&path())
        .map(|kept| kept.key)
        .unwrap_or_default();
    let in_vault = crate::vault::get(KEY).ok().flatten();
    [in_file].into_iter().chain(in_vault).collect()
}

/// This computer's name, as a phone lists it: the one the system goes by on
/// the network, less the `.local` macOS puts after it.
pub fn this_computer() -> String {
    #[cfg(windows)]
    let name = std::env::var("COMPUTERNAME").unwrap_or_default();
    #[cfg(unix)]
    let name = {
        let mut bytes = [0u8; 256];
        // SAFETY: the buffer is as long as the length given, and is ended
        // with a nul if the name fills it.
        let got = unsafe { libc::gethostname(bytes.as_mut_ptr().cast(), bytes.len() - 1) };
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(0);
        match got {
            0 => String::from_utf8_lossy(&bytes[..end]).into_owned(),
            _ => String::new(),
        }
    };
    let name = clean(name.trim_end_matches(".local"));
    match name.is_empty() {
        true => "This computer".into(),
        false => name,
    }
}

fn decode(text: &str) -> Option<[u8; noise::LEN]> {
    URL_SAFE_NO_PAD.decode(text.trim()).ok()?.try_into().ok()
}

/// A QR code the window shows for pairing a phone.
pub struct Invite {
    secret: [u8; noise::LEN],
    /// This computer's public key, which the code carries.
    computer: [u8; noise::LEN],
    /// Where a phone on the same tailnet can reach it, `address:port`.
    direct: Option<String>,
    made: Instant,
}

impl Invite {
    /// A new code, making this computer's key if it has none yet.
    pub fn new() -> Result<Self, String> {
        Ok(Invite {
            secret: noise::random()?,
            computer: computer()?.public(),
            direct: None,
            made: Instant::now(),
        })
    }

    /// Let a phone pair over the tailnet too, at `at`, the address and port
    /// this computer answers on there (`direct::Direct::Answering`).
    pub fn reached_at(mut self, at: &str) -> Self {
        self.direct = Some(at.to_owned());
        self
    }

    /// What the QR code holds.
    pub fn link(&self) -> String {
        let mut link = format!(
            "letronna://pair?v=1&k={}&s={}",
            URL_SAFE_NO_PAD.encode(self.computer),
            URL_SAFE_NO_PAD.encode(self.secret)
        );
        if let Some(at) = &self.direct {
            link.push_str("&d=");
            link.push_str(at);
        }
        link
    }

    /// The QR code of `link`, for the window or the terminal to draw.
    pub fn qr(&self) -> Result<Qr, String> {
        use qrcodegen::{QrCode, QrCodeEcc};
        let code = QrCode::encode_text(&self.link(), QrCodeEcc::Medium)
            .map_err(|_| "The pairing link is too long for a QR code.".to_owned())?;
        let size = code.size();
        let dark = (0..size)
            .flat_map(|y| (0..size).map(move |x| (x, y)))
            .map(|(x, y)| code.get_module(x, y))
            .collect();
        Ok(Qr {
            size: size as usize,
            dark,
        })
    }

    /// Whether it can still be used. It is good for one phone however long
    /// is left, and a wrong try does not use it up: its secret is too long
    /// to guess, so a try that fails is a stale code or noise, and voiding
    /// it would only make the user scan again.
    pub fn live(&self, now: Instant) -> bool {
        now.duration_since(self.made) < LIFE
    }
}

/// A QR code as squares, `size` on a side, row by row. Drawn with a quiet
/// margin of four squares round it, as scanners expect.
pub struct Qr {
    pub size: usize,
    dark: Vec<bool>,
}

impl Qr {
    pub fn dark(&self, x: usize, y: usize) -> bool {
        x < self.size && y < self.size && self.dark[y * self.size + x]
    }
}

/// Who is at the other end of a tunnel.
#[derive(Clone, Debug, PartialEq)]
pub enum Caller {
    /// A phone paired before, as listed.
    Paired(Phone),
    /// A phone that used the QR code. It is kept only once the user says
    /// yes, having checked that `code` is the six digits on the phone in
    /// their hand; until then nothing it sends is read.
    New {
        name: String,
        key: String,
        code: String,
    },
}

/// A phone's first frame, answered.
struct Answered {
    /// The frame to send back.
    reply: Vec<u8>,
    caller: Caller,
    tunnel: Tunnel,
}

/// What a phone or this computer says of itself in the handshake: its
/// name, and from this computer, where a phone can reach it without the
/// relay (`direct`).
#[derive(Default, Deserialize, Serialize)]
struct Hello {
    #[serde(default)]
    name: String,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    direct: Vec<String>,
}

/// Answer a phone's first frame, as this computer with key `me` and the
/// handshake key `e`. A phone pairing needs `invite`, the code the window
/// shows, and uses it up, so no second phone can; one already paired needs
/// to be among `phones`. `said` is what this computer says of itself.
fn answer(
    first: &[u8],
    me: &Keys,
    phones: &[Phone],
    invite: &mut Option<Invite>,
    said: &Hello,
    e: Keys,
    now: Instant,
) -> Result<Answered, String> {
    let (&kind, message) = first.split_first().ok_or("The phone sent nothing.")?;
    let psk = match kind {
        PAIR => {
            let held = invite.as_ref().filter(|held| held.live(now)).ok_or(
                "No phone is being paired now: the QR code was used, ran out, or was closed.",
            )?;
            Some(held.secret)
        }
        PAIRED => None,
        _ => {
            return Err(
                "The phone asked in a way this Letronna does not know; one of the two needs \
                 updating."
                    .into(),
            );
        }
    };
    let mut handshake = Handshake::responder(PROLOGUE, me.clone(), psk, e);
    let hello: Hello = serde_json::from_slice(&handshake.read(message)?).unwrap_or_default();
    let key = handshake
        .remote()
        .map(|key| URL_SAFE_NO_PAD.encode(key))
        .ok_or("The phone sent no key.")?;
    let listed = match psk {
        Some(_) => None,
        None => Some(
            phones
                .iter()
                .find(|phone| phone.key == key)
                .cloned()
                .ok_or("That phone is not paired with this computer.")?,
        ),
    };
    let said = serde_json::to_vec(said).map_err(|e| e.to_string())?;
    let reply = handshake.write(&said)?;
    let transport = handshake.finish()?;
    let given = clean(&hello.name);
    let caller = match listed {
        None => {
            *invite = None;
            Caller::New {
                name: given,
                key,
                code: code(&transport.hash()),
            }
        }
        Some(mut phone) => {
            if !given.is_empty() {
                phone.name = given;
            }
            Caller::Paired(phone)
        }
    };
    Ok(Answered {
        reply,
        caller,
        tunnel: Tunnel::new(transport),
    })
}

/// A phone's name as it may be shown: one line, without control
/// characters, cut short.
fn clean(name: &str) -> String {
    let one_line: String = name
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .collect();
    one_line
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .chars()
        .take(NAME_CHARS)
        .collect()
}

/// The six digits both screens show while pairing.
fn code(hash: &[u8; noise::LEN]) -> String {
    let mut first = [0u8; 8];
    first.copy_from_slice(&hash[..8]);
    format!("{:06}", u64::from_be_bytes(first) % 1_000_000)
}

/// A phone's connection once the handshake is done: messages of any size up
/// to `LONGEST`, encrypted, in pieces that each fit one Noise message.
struct Tunnel {
    noise: Transport,
    /// What has come of the message now arriving.
    coming: Vec<u8>,
}

impl Tunnel {
    fn new(noise: Transport) -> Self {
        Tunnel {
            noise,
            coming: Vec::new(),
        }
    }

    /// The frames that carry `message`, to be sent in order.
    fn seal(&mut self, message: &[u8]) -> Result<Vec<Vec<u8>>, String> {
        if message.len() > LONGEST {
            return Err("A message for the phone is too long to send.".into());
        }
        // An empty message is still sent, as one empty piece.
        let pieces: Vec<&[u8]> = match message.is_empty() {
            true => vec![message],
            false => message.chunks(PIECE).collect(),
        };
        let last = pieces.len() - 1;
        let mut frames = Vec::with_capacity(pieces.len());
        for (index, piece) in pieces.into_iter().enumerate() {
            let mut plain = Vec::with_capacity(piece.len() + 1);
            plain.push(if index < last { MORE } else { LAST });
            plain.extend_from_slice(piece);
            frames.push(self.noise.seal(&plain)?);
        }
        Ok(frames)
    }

    /// Take `frame`, and give the message it ends once that is whole. An
    /// error ends the tunnel.
    fn open(&mut self, frame: &[u8]) -> Result<Option<Vec<u8>>, String> {
        let plain = self.noise.open(frame)?;
        let (&flag, piece) = plain
            .split_first()
            .ok_or("The phone sent an empty piece.")?;
        if self.coming.len() + piece.len() > LONGEST {
            return Err("The phone sent a message too long to take.".into());
        }
        self.coming.extend_from_slice(piece);
        match flag {
            LAST => Ok(Some(std::mem::take(&mut self.coming))),
            MORE => Ok(None),
            _ => Err("The phone sent a piece this Letronna cannot place.".into()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::tests::{DATA_DIR, Scratch};
    use std::sync::MutexGuard;

    /// A phone's side of a handshake: its first frame, and the handshake to
    /// read the answer with.
    fn call(
        kind: u8,
        phone: &Keys,
        computer: [u8; noise::LEN],
        psk: Option<[u8; noise::LEN]>,
    ) -> (Handshake, Vec<u8>) {
        let mut handshake = Handshake::initiator(
            PROLOGUE,
            phone.clone(),
            computer,
            psk,
            Keys::random().unwrap(),
        );
        let mut first = vec![kind];
        first.extend(handshake.write(br#"{"name": "Pocket"}"#).unwrap());
        (handshake, first)
    }

    /// The phone reads the answer: this computer's name, and its tunnel.
    fn hear(mut handshake: Handshake, reply: &[u8]) -> (String, Tunnel) {
        let hello: Hello = serde_json::from_slice(&handshake.read(reply).unwrap()).unwrap();
        (hello.name, Tunnel::new(handshake.finish().unwrap()))
    }

    fn desk() -> Hello {
        Hello {
            name: "Desk".into(),
            direct: Vec::new(),
        }
    }

    fn invite(computer: &Keys, made: Instant) -> Invite {
        Invite {
            secret: noise::random().unwrap(),
            computer: computer.public(),
            direct: None,
            made,
        }
    }

    fn listed(phone: &Keys, name: &str) -> Phone {
        Phone {
            name: name.into(),
            key: URL_SAFE_NO_PAD.encode(phone.public()),
            paired: 1,
            seen: 1,
        }
    }

    #[test]
    fn a_phone_with_the_codes_secret_pairs_and_both_sides_show_the_same_digits() {
        let computer = Keys::random().unwrap();
        let phone = Keys::random().unwrap();
        let now = Instant::now();
        let mut shown = Some(invite(&computer, now));
        let secret = shown.as_ref().unwrap().secret;

        let (handshake, first) = call(PAIR, &phone, computer.public(), Some(secret));
        let answered = answer(
            &first,
            &computer,
            &[],
            &mut shown,
            &desk(),
            Keys::random().unwrap(),
            now,
        )
        .unwrap();
        let (name, _) = hear(handshake, &answered.reply);
        assert_eq!(name, "Desk");

        let Caller::New { name, key, code } = &answered.caller else {
            panic!("a pairing phone is new");
        };
        assert_eq!(name, "Pocket");
        assert_eq!(key, &URL_SAFE_NO_PAD.encode(phone.public()));
        assert_eq!(code.len(), 6);
        assert!(code.bytes().all(|b| b.is_ascii_digit()));
        // Used up: no second phone can pair with it.
        assert!(shown.is_none());
    }

    #[test]
    fn the_phone_works_out_the_same_six_digits() {
        let computer = Keys::random().unwrap();
        let phone = Keys::random().unwrap();
        let now = Instant::now();
        let mut shown = Some(invite(&computer, now));
        let secret = shown.as_ref().unwrap().secret;
        let (mut handshake, first) = call(PAIR, &phone, computer.public(), Some(secret));
        let answered = answer(
            &first,
            &computer,
            &[],
            &mut shown,
            &desk(),
            Keys::random().unwrap(),
            now,
        )
        .unwrap();
        handshake.read(&answered.reply).unwrap();
        let on_phone = code(&handshake.finish().unwrap().hash());
        assert!(matches!(answered.caller, Caller::New { code, .. } if code == on_phone));
    }

    #[test]
    fn a_wrong_secret_is_turned_away_and_leaves_the_code_for_the_right_phone() {
        let computer = Keys::random().unwrap();
        let now = Instant::now();
        let mut shown = Some(invite(&computer, now));
        let (_, first) = call(
            PAIR,
            &Keys::random().unwrap(),
            computer.public(),
            Some([7; 32]),
        );
        let e = Keys::random().unwrap();
        assert!(answer(&first, &computer, &[], &mut shown, &desk(), e, now).is_err());
        assert!(shown.is_some());
    }

    #[test]
    fn a_code_used_or_run_out_pairs_nobody() {
        let computer = Keys::random().unwrap();
        let made = Instant::now();
        let secret = invite(&computer, made).secret;
        let (_, first) = call(
            PAIR,
            &Keys::random().unwrap(),
            computer.public(),
            Some(secret),
        );
        let e = || Keys::random().unwrap();

        let mut used = None;
        assert!(answer(&first, &computer, &[], &mut used, &desk(), e(), made).is_err());

        let mut old = Some(Invite {
            secret,
            ..invite(&computer, made)
        });
        let later = made + LIFE;
        assert!(answer(&first, &computer, &[], &mut old, &desk(), e(), later).is_err());
    }

    #[test]
    fn a_paired_phone_connects_and_is_known_by_its_key() {
        let computer = Keys::random().unwrap();
        let phone = Keys::random().unwrap();
        let phones = [
            listed(&Keys::random().unwrap(), "Other"),
            listed(&phone, "Old name"),
        ];
        let (handshake, first) = call(PAIRED, &phone, computer.public(), None);
        let now = Instant::now();
        let answered = answer(
            &first,
            &computer,
            &phones,
            &mut None,
            &desk(),
            Keys::random().unwrap(),
            now,
        )
        .unwrap();
        hear(handshake, &answered.reply);
        let Caller::Paired(known) = answered.caller else {
            panic!("a listed phone is paired");
        };
        assert_eq!(known.key, phones[1].key);
        assert_eq!(known.name, "Pocket");
    }

    #[test]
    fn a_phone_not_in_the_list_is_turned_away() {
        let computer = Keys::random().unwrap();
        let phones = [listed(&Keys::random().unwrap(), "Other")];
        let (_, first) = call(PAIRED, &Keys::random().unwrap(), computer.public(), None);
        let e = Keys::random().unwrap();
        let now = Instant::now();
        assert!(answer(&first, &computer, &phones, &mut None, &desk(), e, now).is_err());
    }

    #[test]
    fn a_phone_cannot_pair_by_saying_it_is_already_paired() {
        // The pairing secret is what lets a new key in; a frame that skips
        // it has to name a key already listed.
        let computer = Keys::random().unwrap();
        let now = Instant::now();
        let mut shown = Some(invite(&computer, now));
        let (_, first) = call(PAIRED, &Keys::random().unwrap(), computer.public(), None);
        let e = Keys::random().unwrap();
        assert!(answer(&first, &computer, &[], &mut shown, &desk(), e, now).is_err());
        assert!(shown.is_some());
    }

    #[test]
    fn a_frame_of_an_unknown_kind_is_turned_away() {
        let computer = Keys::random().unwrap();
        let (_, mut first) = call(PAIRED, &Keys::random().unwrap(), computer.public(), None);
        first[0] = 9;
        let e = Keys::random().unwrap();
        let now = Instant::now();
        assert!(answer(&first, &computer, &[], &mut None, &desk(), e, now).is_err());
        assert!(
            answer(
                &[],
                &computer,
                &[],
                &mut None,
                &desk(),
                Keys::random().unwrap(),
                now
            )
            .is_err()
        );
    }

    /// A tunnel each way between a paired phone and this computer.
    fn tunnels() -> (Tunnel, Tunnel) {
        let computer = Keys::random().unwrap();
        let phone = Keys::random().unwrap();
        let (handshake, first) = call(PAIRED, &phone, computer.public(), None);
        let phones = [listed(&phone, "Pocket")];
        let now = Instant::now();
        let answered = answer(
            &first,
            &computer,
            &phones,
            &mut None,
            &desk(),
            Keys::random().unwrap(),
            now,
        )
        .unwrap();
        (hear(handshake, &answered.reply).1, answered.tunnel)
    }

    fn deliver(from: &mut Tunnel, to: &mut Tunnel, message: &[u8]) -> Vec<u8> {
        let frames = from.seal(message).unwrap();
        let (last, rest) = frames.split_last().unwrap();
        for frame in rest {
            assert_eq!(to.open(frame).unwrap(), None);
        }
        to.open(last).unwrap().unwrap()
    }

    #[test]
    fn messages_of_any_size_cross_both_ways() {
        let (mut phone, mut computer) = tunnels();
        let long: Vec<u8> = (0..PIECE * 3 + 5).map(|i| i as u8).collect();
        for message in [&b""[..], b"{}", &long[..PIECE], &long[..PIECE + 1], &long] {
            assert_eq!(deliver(&mut phone, &mut computer, message), message);
            assert_eq!(deliver(&mut computer, &mut phone, message), message);
        }
        assert_eq!(phone.seal(&long).unwrap().len(), 4);
        assert_eq!(phone.seal(&long[..PIECE]).unwrap().len(), 1);
    }

    #[test]
    fn a_changed_frame_ends_the_tunnel() {
        let (mut phone, mut computer) = tunnels();
        let mut frame = phone.seal(b"stop").unwrap().remove(0);
        let last = frame.len() - 1;
        frame[last] ^= 1;
        assert!(computer.open(&frame).is_err());
    }

    #[test]
    fn a_message_too_long_is_refused_on_both_sides() {
        let (mut phone, mut computer) = tunnels();
        assert!(phone.seal(&vec![0; LONGEST + 1]).is_err());
        // A phone that sends more pieces than any message may have.
        let piece = vec![MORE; PIECE + 1];
        let mut refused = false;
        for _ in 0..=LONGEST / PIECE {
            let frame = phone.noise.seal(&piece).unwrap();
            if computer.open(&frame).is_err() {
                refused = true;
                break;
            }
        }
        assert!(refused);
    }

    #[test]
    fn a_name_is_shown_on_one_line_and_cut_short() {
        assert_eq!(clean("  Hafizh's\n\tiPhone\u{7}  "), "Hafizh's iPhone");
        assert_eq!(clean(&"x".repeat(200)).chars().count(), NAME_CHARS);
    }

    #[test]
    fn the_qr_code_is_a_whole_one_with_its_three_corner_marks() {
        let qr = invite(&Keys::random().unwrap(), Instant::now())
            .qr()
            .unwrap();
        // Version 7 at the medium level holds the link, 45 squares a side.
        assert_eq!(qr.size, 45);
        let last = qr.size - 1;
        for (x, y) in [(0, 0), (last, 0), (0, last)] {
            assert!(qr.dark(x, y), "no mark at {x},{y}");
        }
        // The fourth corner has none: that is how a scanner turns it.
        assert!(!(qr.dark(last, last) && qr.dark(last - 6, last) && qr.dark(last, last - 6)));
        assert!(!qr.dark(qr.size, 0));
    }

    #[test]
    fn the_code_carries_the_key_and_the_secret() {
        let computer = Keys::random().unwrap();
        let shown = invite(&computer, Instant::now());
        let link = shown.link();
        let query = link.strip_prefix("letronna://pair?v=1&").unwrap();
        let (k, s) = query.split_once('&').unwrap();
        assert_eq!(
            decode(k.strip_prefix("k=").unwrap()),
            Some(computer.public())
        );
        assert_eq!(decode(s.strip_prefix("s=").unwrap()), Some(shown.secret));
        // And where a phone on the tailnet can pair, when it can.
        let shown = shown.reached_at("100.101.102.103:47821");
        assert_eq!(
            shown.link(),
            format!("{link}&d=100.101.102.103:47821"),
            "the address goes last, as it is"
        );
        assert!(shown.qr().is_ok());
    }

    /// A data folder of this test's own, held from other tests, with an
    /// empty vault on this thread.
    fn data(
        name: &str,
    ) -> (
        MutexGuard<'static, ()>,
        Scratch,
        std::sync::Arc<crate::vault::tests::Fake>,
    ) {
        let held = DATA_DIR.lock().unwrap_or_else(|e| e.into_inner());
        let dir = Scratch::new(name);
        unsafe { std::env::set_var("AGENT_DATA_DIR", &*dir) };
        let vault = crate::vault::tests::fake();
        (held, dir, vault)
    }

    #[test]
    fn the_computers_key_is_made_once_and_kept_in_the_vault() {
        let (_held, _dir, vault) = data("phone-key");
        let made = computer().unwrap();
        assert_eq!(computer().unwrap().public(), made.public());
        assert!(vault.holds(KEY).is_some());
        let file: Value = serde_json::from_slice(&std::fs::read(path()).unwrap()).unwrap();
        assert_eq!(file["key"], "");
        assert!(secrets().contains(&URL_SAFE_NO_PAD.encode(made.secret())));
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn a_vault_that_will_not_answer_never_gets_a_new_key_made_around_it() {
        let (_held, _dir, vault) = data("phone-key-locked");
        let made = computer().unwrap();
        // As the next start finds it, with nothing read from it yet.
        crate::vault::tests::restart(&vault);
        vault
            .locked
            .store(true, std::sync::atomic::Ordering::SeqCst);
        assert!(computer().is_err());
        assert!(Invite::new().is_err());
        // And the same key is there once a later start finds it unlocked.
        vault
            .locked
            .store(false, std::sync::atomic::Ordering::SeqCst);
        crate::vault::tests::restart(&vault);
        assert_eq!(computer().unwrap().public(), made.public());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }

    #[test]
    fn phones_are_kept_seen_and_forgotten_without_touching_the_key() {
        let (_held, _dir, vault) = data("phone-list");
        let made = computer().unwrap();
        keep("Pocket", "AAAA").unwrap();
        keep("Tablet", "BBBB").unwrap();
        // Pairing the same phone again replaces it rather than listing it twice.
        keep("Pocket again", "AAAA").unwrap();
        let names: Vec<String> = phones().into_iter().map(|phone| phone.name).collect();
        assert_eq!(names, ["Tablet", "Pocket again"]);

        seen("BBBB", "Renamed").unwrap();
        assert_eq!(phones()[0].name, "Renamed");
        forget("BBBB").unwrap();
        assert_eq!(phones().len(), 1);

        assert_eq!(
            vault.holds(KEY),
            Some(URL_SAFE_NO_PAD.encode(made.secret()))
        );
        assert_eq!(computer().unwrap().public(), made.public());
        unsafe { std::env::remove_var("AGENT_DATA_DIR") };
    }
}
