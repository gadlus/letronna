//! The Noise Protocol Framework (revision 34), as much of it as the phone's
//! remote needs: the IK handshake, alone or with a pre-shared key after its
//! first message (IKpsk1), on X25519, ChaCha20-Poly1305 and SHA-256, and the
//! pair of ciphers it ends in.
//!
//! Written out here rather than taken from `snow`: ring already has the
//! cipher, the hash and HMAC, and curve25519-dalek, here for the wallet, has
//! X25519, which ring does only with a key it made itself and uses once.
//! `snow` would add a second curve25519-dalek and RustCrypto's ciphers
//! beside ring's, for the same three functions. The phone follows the same
//! spec, so both sides answer to the published test vectors, cacophony's,
//! which the tests at the bottom run.

use curve25519_dalek::montgomery::MontgomeryPoint;
use ring::aead::{Aad, CHACHA20_POLY1305, LessSafeKey, Nonce, UnboundKey};
use ring::{digest, hmac};

/// Bytes in a key, in a hash, and in what two keys agree on.
pub const LEN: usize = 32;
/// Bytes the cipher's tag adds to what it encrypts.
pub const TAG: usize = 16;
/// The most a Noise message may be, its tag included.
pub const MOST: usize = 65535;

/// What any message that fails its check is told as. Which check failed is
/// not said: it would only help whoever made the message.
const FAILED: &str = "A message from the phone failed its check: it was changed on the way, \
or not made with the keys this computer expects.";

/// An X25519 key pair.
#[derive(Clone)]
pub struct Keys {
    secret: [u8; LEN],
    public: [u8; LEN],
}

impl Keys {
    /// The pair whose secret half is `secret`.
    pub fn from_secret(secret: [u8; LEN]) -> Self {
        let public = MontgomeryPoint::mul_base_clamped(secret).to_bytes();
        Keys { secret, public }
    }

    /// A new pair from the system's random source.
    pub fn random() -> Result<Self, String> {
        Ok(Self::from_secret(random()?))
    }

    pub fn public(&self) -> [u8; LEN] {
        self.public
    }

    /// The secret half, to be kept.
    pub fn secret(&self) -> [u8; LEN] {
        self.secret
    }
}

/// Thirty-two bytes from the system's random source.
pub fn random() -> Result<[u8; LEN], String> {
    let mut bytes = [0u8; LEN];
    ring::rand::SecureRandom::fill(&ring::rand::SystemRandom::new(), &mut bytes)
        .map_err(|_| "The system's random source failed, so no key was made.".to_owned())?;
    Ok(bytes)
}

/// What `secret` and `public` agree on. A public key of small order agrees
/// on zeros with every secret, which would let whoever sent it decide the
/// outcome without holding any key, so one that does is refused.
fn dh(secret: &[u8; LEN], public: &[u8; LEN]) -> Result<[u8; LEN], String> {
    let shared = MontgomeryPoint(*public).mul_clamped(*secret).to_bytes();
    if shared == [0; LEN] {
        return Err(FAILED.into());
    }
    Ok(shared)
}

fn array(bytes: &[u8]) -> [u8; LEN] {
    let mut out = [0u8; LEN];
    out.copy_from_slice(bytes);
    out
}

/// Noise's HKDF: three keys from the chaining key and `input`. A caller
/// that wants two takes the first two, which do not depend on the third.
fn hkdf(chaining: &[u8; LEN], input: &[u8]) -> [[u8; LEN]; 3] {
    let temp = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, chaining), input);
    let temp = hmac::Key::new(hmac::HMAC_SHA256, temp.as_ref());
    let next = |previous: &[u8], index: u8| {
        let mut context = hmac::Context::with_key(&temp);
        context.update(previous);
        context.update(&[index]);
        array(context.sign().as_ref())
    };
    let one = next(&[], 1);
    let two = next(&one, 2);
    let three = next(&two, 3);
    [one, two, three]
}

/// A key and how many messages it has done, which is the next one's nonce.
struct Cipher {
    key: Option<LessSafeKey>,
    n: u64,
}

impl Cipher {
    fn none() -> Self {
        Cipher { key: None, n: 0 }
    }

    fn with(key: &[u8; LEN]) -> Result<Self, String> {
        let key = UnboundKey::new(&CHACHA20_POLY1305, key)
            .map_err(|_| "The cipher would not take its key.".to_owned())?;
        Ok(Cipher {
            key: Some(LessSafeKey::new(key)),
            n: 0,
        })
    }

    /// The nonce for the next message: four zero bytes, then the count,
    /// little-endian. The last count is never used, as the spec keeps it
    /// back, so no nonce can come round twice.
    fn nonce(&self) -> Result<Nonce, String> {
        if self.n == u64::MAX {
            return Err("This connection has carried all it may; it has to start again.".into());
        }
        let mut bytes = [0u8; 12];
        bytes[4..].copy_from_slice(&self.n.to_le_bytes());
        Ok(Nonce::assume_unique_for_key(bytes))
    }

    /// `plain` encrypted, bound to `ad`; as it is, before there is a key.
    fn encrypt(&mut self, ad: &[u8], plain: &[u8]) -> Result<Vec<u8>, String> {
        let Some(key) = &self.key else {
            return Ok(plain.to_vec());
        };
        let mut out = plain.to_vec();
        key.seal_in_place_append_tag(self.nonce()?, Aad::from(ad), &mut out)
            .map_err(|_| "A message could not be encrypted.".to_owned())?;
        self.n += 1;
        Ok(out)
    }

    /// What `sealed` holds, if it was made with this key and `ad`. One that
    /// was not leaves the count alone, as the spec has it.
    fn decrypt(&mut self, ad: &[u8], sealed: &[u8]) -> Result<Vec<u8>, String> {
        let Some(key) = &self.key else {
            return Ok(sealed.to_vec());
        };
        let mut out = sealed.to_vec();
        let plain = key
            .open_in_place(self.nonce()?, Aad::from(ad), &mut out)
            .map_err(|_| FAILED.to_owned())?
            .len();
        out.truncate(plain);
        self.n += 1;
        Ok(out)
    }
}

/// The handshake's chaining key and running hash, and the cipher they give.
struct Symmetric {
    chaining: [u8; LEN],
    hash: [u8; LEN],
    cipher: Cipher,
}

impl Symmetric {
    fn new(protocol: &str) -> Self {
        let name = protocol.as_bytes();
        let hash = if name.len() <= LEN {
            let mut padded = [0u8; LEN];
            padded[..name.len()].copy_from_slice(name);
            padded
        } else {
            array(digest::digest(&digest::SHA256, name).as_ref())
        };
        Symmetric {
            chaining: hash,
            hash,
            cipher: Cipher::none(),
        }
    }

    fn mix_key(&mut self, input: &[u8]) -> Result<(), String> {
        let [chaining, key, _] = hkdf(&self.chaining, input);
        self.chaining = chaining;
        self.cipher = Cipher::with(&key)?;
        Ok(())
    }

    fn mix_hash(&mut self, data: &[u8]) {
        let mut context = digest::Context::new(&digest::SHA256);
        context.update(&self.hash);
        context.update(data);
        self.hash = array(context.finish().as_ref());
    }

    fn mix_key_and_hash(&mut self, input: &[u8]) -> Result<(), String> {
        let [chaining, hash, key] = hkdf(&self.chaining, input);
        self.chaining = chaining;
        self.mix_hash(&hash);
        self.cipher = Cipher::with(&key)?;
        Ok(())
    }

    fn encrypt_and_hash(&mut self, plain: &[u8]) -> Result<Vec<u8>, String> {
        let sealed = self.cipher.encrypt(&self.hash, plain)?;
        self.mix_hash(&sealed);
        Ok(sealed)
    }

    fn decrypt_and_hash(&mut self, sealed: &[u8]) -> Result<Vec<u8>, String> {
        let plain = self.cipher.decrypt(&self.hash, sealed)?;
        self.mix_hash(sealed);
        Ok(plain)
    }

    /// The two ciphers for after the handshake: the initiator's sending
    /// one first.
    fn split(&self) -> Result<(Cipher, Cipher), String> {
        let [first, second, _] = hkdf(&self.chaining, &[]);
        Ok((Cipher::with(&first)?, Cipher::with(&second)?))
    }
}

/// What a handshake message does, in order: send a key, mix in what two
/// keys agree on, or mix in the pre-shared key.
#[derive(Clone, Copy, PartialEq)]
enum Token {
    E,
    S,
    Ee,
    Es,
    Se,
    Ss,
    Psk,
}

/// IK's two messages. The initiator knows the responder's key before it
/// starts and sends its own encrypted to it, so a handshake is one message
/// each way.
const IK: [&[Token]; 2] = [
    &[Token::E, Token::Es, Token::S, Token::Ss],
    &[Token::E, Token::Ee, Token::Se],
];

/// IK with a pre-shared key mixed in at the end of the first message, so
/// the responder can tell from that message alone whether the initiator
/// holds it, and answers nobody who does not.
const IK_PSK1: [&[Token]; 2] = [
    &[Token::E, Token::Es, Token::S, Token::Ss, Token::Psk],
    &[Token::E, Token::Ee, Token::Se],
];

/// One side of an IK handshake.
pub struct Handshake {
    symmetric: Symmetric,
    pattern: [&'static [Token]; 2],
    initiator: bool,
    psk: Option<[u8; LEN]>,
    s: Keys,
    /// Made before the handshake starts rather than when it is sent, so a
    /// test can give the one a test vector uses.
    e: Keys,
    rs: Option<[u8; LEN]>,
    re: Option<[u8; LEN]>,
    /// Messages written and read so far.
    done: usize,
}

impl Handshake {
    /// The side that starts, knowing the other's key `remote`; IKpsk1 with
    /// `psk`, IK without. That is the phone's, so only the tests start one
    /// here.
    #[cfg(test)]
    pub fn initiator(
        prologue: &[u8],
        s: Keys,
        remote: [u8; LEN],
        psk: Option<[u8; LEN]>,
        e: Keys,
    ) -> Self {
        Self::new(prologue, true, s, Some(remote), psk, e)
    }

    /// The side that answers, which learns the other's key from its first
    /// message.
    pub fn responder(prologue: &[u8], s: Keys, psk: Option<[u8; LEN]>, e: Keys) -> Self {
        Self::new(prologue, false, s, None, psk, e)
    }

    fn new(
        prologue: &[u8],
        initiator: bool,
        s: Keys,
        rs: Option<[u8; LEN]>,
        psk: Option<[u8; LEN]>,
        e: Keys,
    ) -> Self {
        let (pattern, protocol) = match psk {
            Some(_) => (IK_PSK1, "Noise_IKpsk1_25519_ChaChaPoly_SHA256"),
            None => (IK, "Noise_IK_25519_ChaChaPoly_SHA256"),
        };
        let mut symmetric = Symmetric::new(protocol);
        symmetric.mix_hash(prologue);
        // IK's pre-message: the responder's key, which both have already.
        symmetric.mix_hash(&rs.unwrap_or(s.public));
        Handshake {
            symmetric,
            pattern,
            initiator,
            psk,
            s,
            e,
            rs,
            re: None,
            done: 0,
        }
    }

    /// Whether the next message is this side's to write.
    fn writes(&self) -> bool {
        self.done.is_multiple_of(2) == self.initiator
    }

    /// The next message, carrying `payload`.
    pub fn write(&mut self, payload: &[u8]) -> Result<Vec<u8>, String> {
        if self.done >= self.pattern.len() || !self.writes() {
            return Err("The handshake was asked to write out of turn.".into());
        }
        let mut out = Vec::new();
        for &token in self.pattern[self.done] {
            match token {
                Token::E => {
                    let e = self.e.public;
                    self.symmetric.mix_hash(&e);
                    if self.psk.is_some() {
                        self.symmetric.mix_key(&e)?;
                    }
                    out.extend_from_slice(&e);
                }
                Token::S => {
                    let s = self.s.public;
                    out.extend(self.symmetric.encrypt_and_hash(&s)?);
                }
                other => self.mix(other)?,
            }
        }
        out.extend(self.symmetric.encrypt_and_hash(payload)?);
        if out.len() > MOST {
            return Err("A handshake message would be longer than Noise allows.".into());
        }
        self.done += 1;
        Ok(out)
    }

    /// The payload of the other side's next message.
    pub fn read(&mut self, message: &[u8]) -> Result<Vec<u8>, String> {
        if self.done >= self.pattern.len() || self.writes() {
            return Err("The handshake was asked to read out of turn.".into());
        }
        if message.len() > MOST {
            return Err(FAILED.into());
        }
        let mut rest = message;
        for &token in self.pattern[self.done] {
            match token {
                Token::E => {
                    let (re, after) = take(rest, LEN)?;
                    self.symmetric.mix_hash(re);
                    if self.psk.is_some() {
                        self.symmetric.mix_key(re)?;
                    }
                    self.re = Some(array(re));
                    rest = after;
                }
                Token::S => {
                    let len = match self.symmetric.cipher.key {
                        Some(_) => LEN + TAG,
                        None => LEN,
                    };
                    let (sealed, after) = take(rest, len)?;
                    self.rs = Some(array(&self.symmetric.decrypt_and_hash(sealed)?));
                    rest = after;
                }
                other => self.mix(other)?,
            }
        }
        let payload = self.symmetric.decrypt_and_hash(rest)?;
        self.done += 1;
        Ok(payload)
    }

    /// Mix in what two keys agree on, or the pre-shared key.
    fn mix(&mut self, token: Token) -> Result<(), String> {
        if token == Token::Psk {
            let psk = self.psk.ok_or("The handshake has no pre-shared key.")?;
            return self.symmetric.mix_key_and_hash(&psk);
        }
        let (mine, theirs) = match (token, self.initiator) {
            (Token::Ee, _) => (&self.e, self.re),
            (Token::Es, true) | (Token::Se, false) => (&self.e, self.rs),
            (Token::Es, false) | (Token::Se, true) => (&self.s, self.re),
            (Token::Ss, _) => (&self.s, self.rs),
            _ => return Err("The handshake met a step it does not know.".into()),
        };
        let theirs = theirs.ok_or("The handshake is missing the other side's key.")?;
        let shared = dh(&mine.secret, &theirs)?;
        self.symmetric.mix_key(&shared)
    }

    /// The other side's key, once its message carrying it has been read.
    pub fn remote(&self) -> Option<[u8; LEN]> {
        self.rs
    }

    /// The ciphers for the rest of the connection, once both messages are
    /// done.
    pub fn finish(self) -> Result<Transport, String> {
        if self.done < self.pattern.len() {
            return Err("The handshake is not finished.".into());
        }
        let (first, second) = self.symmetric.split()?;
        let (send, receive) = match self.initiator {
            true => (first, second),
            false => (second, first),
        };
        Ok(Transport {
            send,
            receive,
            hash: self.symmetric.hash,
        })
    }
}

/// The first `len` bytes of `bytes`, and the rest.
fn take(bytes: &[u8], len: usize) -> Result<(&[u8], &[u8]), String> {
    match bytes.len() >= len {
        true => Ok(bytes.split_at(len)),
        false => Err(FAILED.into()),
    }
}

/// A finished handshake: a cipher each way, and the hash of everything the
/// handshake said, which is the same on both sides only when both made the
/// same handshake.
pub struct Transport {
    send: Cipher,
    receive: Cipher,
    hash: [u8; LEN],
}

impl Transport {
    /// `plain`, encrypted as the next message out.
    pub fn seal(&mut self, plain: &[u8]) -> Result<Vec<u8>, String> {
        if plain.len() + TAG > MOST {
            return Err("A message would be longer than Noise allows.".into());
        }
        self.send.encrypt(&[], plain)
    }

    /// What the next message in holds.
    pub fn open(&mut self, sealed: &[u8]) -> Result<Vec<u8>, String> {
        if sealed.len() > MOST {
            return Err(FAILED.into());
        }
        self.receive.decrypt(&[], sealed)
    }

    pub fn hash(&self) -> [u8; LEN] {
        self.hash
    }
}

#[cfg(test)]
mod tests {
    use super::{Handshake, Keys, LEN, MOST, TAG, array};

    fn hex(text: &str) -> Vec<u8> {
        (0..text.len())
            .step_by(2)
            .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
            .collect()
    }

    fn key(text: &str) -> [u8; LEN] {
        array(&hex(text))
    }

    /// A handshake and the messages after it, as cacophony writes them.
    struct Vector {
        prologue: &'static str,
        psk: Option<&'static str>,
        init_static: &'static str,
        init_ephemeral: &'static str,
        resp_static: &'static str,
        resp_ephemeral: &'static str,
        handshake_hash: &'static str,
        /// Payload and ciphertext, in hex: the handshake's two, then
        /// transport messages, the initiator's first and then by turns.
        messages: &'static [(&'static str, &'static str)],
    }

    /// Run `vector` with both sides here, checking every message each side
    /// writes is the vector's byte for byte, and that the other reads it.
    fn run(vector: &Vector) {
        let prologue = hex(vector.prologue);
        let psk = vector.psk.map(key);
        let responder = Keys::from_secret(key(vector.resp_static));
        let mut initiator = Handshake::initiator(
            &prologue,
            Keys::from_secret(key(vector.init_static)),
            responder.public(),
            psk,
            Keys::from_secret(key(vector.init_ephemeral)),
        );
        let mut answering = Handshake::responder(
            &prologue,
            responder,
            psk,
            Keys::from_secret(key(vector.resp_ephemeral)),
        );
        let [(payload, sent), (reply, answered), ..] = vector.messages else {
            panic!("a vector has two handshake messages");
        };
        let first = initiator.write(&hex(payload)).unwrap();
        assert_eq!(first, hex(sent));
        assert_eq!(answering.read(&first).unwrap(), hex(payload));
        assert_eq!(
            answering.remote(),
            Some(Keys::from_secret(key(vector.init_static)).public())
        );
        let second = answering.write(&hex(reply)).unwrap();
        assert_eq!(second, hex(answered));
        assert_eq!(initiator.read(&second).unwrap(), hex(reply));

        let mut initiator = initiator.finish().unwrap();
        let mut answering = answering.finish().unwrap();
        assert_eq!(initiator.hash().to_vec(), hex(vector.handshake_hash));
        assert_eq!(answering.hash().to_vec(), hex(vector.handshake_hash));
        for (turn, (payload, sealed)) in vector.messages[2..].iter().enumerate() {
            let (from, to) = match turn % 2 {
                0 => (&mut initiator, &mut answering),
                _ => (&mut answering, &mut initiator),
            };
            let out = from.seal(&hex(payload)).unwrap();
            assert_eq!(out, hex(sealed));
            assert_eq!(to.open(&out).unwrap(), hex(payload));
        }
    }

    #[test]
    fn makes_cacophonys_ik_byte_for_byte() {
        run(&IK);
    }

    #[test]
    fn makes_cacophonys_ik_with_a_pre_shared_key_byte_for_byte() {
        run(&IK_PSK1);
    }

    /// Both sides of a handshake with fresh keys, the responder's known to
    /// the initiator; the initiator's `psk` and the responder's may differ.
    fn pair(sent: Option<[u8; LEN]>, expected: Option<[u8; LEN]>) -> (Handshake, Handshake) {
        let computer = Keys::random().unwrap();
        let initiator = Handshake::initiator(
            b"test",
            Keys::random().unwrap(),
            computer.public(),
            sent,
            Keys::random().unwrap(),
        );
        let responder = Handshake::responder(b"test", computer, expected, Keys::random().unwrap());
        (initiator, responder)
    }

    #[test]
    fn the_responder_refuses_the_first_message_without_the_right_pre_shared_key() {
        let (mut initiator, mut responder) = pair(Some([1; LEN]), Some([2; LEN]));
        let first = initiator.write(b"hello").unwrap();
        assert!(responder.read(&first).is_err());

        // Nor does it take a message made without one when it wants one.
        let (mut initiator, mut responder) = pair(None, Some([2; LEN]));
        let first = initiator.write(b"hello").unwrap();
        assert!(responder.read(&first).is_err());
    }

    #[test]
    fn a_changed_byte_anywhere_fails_the_check() {
        let (mut initiator, _) = pair(None, None);
        let first = initiator.write(b"hello").unwrap();
        for at in 0..first.len() {
            let (_, mut responder) = pair(None, None);
            let mut changed = first.clone();
            changed[at] ^= 1;
            assert!(responder.read(&changed).is_err(), "byte {at}");
        }
    }

    #[test]
    fn a_key_of_small_order_is_refused() {
        let (mut initiator, mut responder) = pair(None, None);
        let mut first = initiator.write(b"hello").unwrap();
        // Zero is on the curve, and of small order: every key agrees on
        // zeros with it.
        first[..LEN].fill(0);
        assert!(responder.read(&first).is_err());
    }

    #[test]
    fn each_side_reads_and_writes_only_in_turn() {
        let (mut initiator, mut responder) = pair(None, None);
        assert!(responder.write(b"").is_err());
        assert!(initiator.read(&[0; 100]).is_err());
        let first = initiator.write(b"").unwrap();
        assert!(initiator.write(b"").is_err());
        responder.read(&first).unwrap();
        let second = responder.write(b"").unwrap();
        assert!(responder.write(b"").is_err());
        initiator.read(&second).unwrap();
        assert!(initiator.read(&second).is_err());
    }

    #[test]
    fn a_handshake_is_not_finished_before_both_messages() {
        let (mut initiator, mut responder) = pair(None, None);
        let first = initiator.write(b"").unwrap();
        responder.read(&first).unwrap();
        assert!(responder.finish().is_err());
    }

    #[test]
    fn a_replayed_or_reordered_transport_message_is_refused() {
        let (mut initiator, mut responder) = pair(None, None);
        let first = initiator.write(b"").unwrap();
        responder.read(&first).unwrap();
        let second = responder.write(b"").unwrap();
        initiator.read(&second).unwrap();
        let mut phone = initiator.finish().unwrap();
        let mut computer = responder.finish().unwrap();

        let one = phone.seal(b"one").unwrap();
        let two = phone.seal(b"two").unwrap();
        assert!(computer.open(&two).is_err());
        assert_eq!(computer.open(&one).unwrap(), b"one");
        assert!(computer.open(&one).is_err());
        assert_eq!(computer.open(&two).unwrap(), b"two");
    }

    #[test]
    fn a_message_longer_than_noise_allows_is_not_sealed() {
        let (mut initiator, mut responder) = pair(None, None);
        let first = initiator.write(b"").unwrap();
        responder.read(&first).unwrap();
        initiator.read(&responder.write(b"").unwrap()).unwrap();
        let mut phone = initiator.finish().unwrap();
        assert!(phone.seal(&vec![0; MOST - TAG + 1]).is_err());
        assert_eq!(phone.seal(&vec![0; MOST - TAG]).unwrap().len(), MOST);
    }

    /// Noise_IK_25519_ChaChaPoly_SHA256, from cacophony.
    const IK: Vector = Vector {
        prologue: "4a6f686e2047616c74",
        psk: None,
        init_static: "e61ef9919cde45dd5f82166404bd08e38bceb5dfdfded0a34c8df7ed542214d1",
        init_ephemeral: "893e28b9dc6ca8d611ab664754b8ceb7bac5117349a4439a6b0569da977c464a",
        resp_static: "4a3acbfdb163dec651dfa3194dece676d437029c62a408b4c5ea9114246e4893",
        resp_ephemeral: "bbdb4cdbd309f1a1f2e1456967fe288cadd6f712d65dc7b7793d5e63da6b375b",
        handshake_hash: "0b0f68fb0c27e03ce9b97565995ed4838cc0581b762ef72b062f6a546419fad7",
        messages: &[
            (
                "4c756477696720766f6e204d69736573",
                "ca35def5ae56cec33dc2036731ab14896bc4c75dbb07a61f879f8e3afa4c7944718da798efbcd91528520204f904b9bd6c7413dccdc214d951e15253e39987f18146e8cd0873654207148333479d4d16c289f0294b29960a72f48e0b7bba2e89083169825e59642148d492020664ccf7",
            ),
            (
                "4d757272617920526f746862617264",
                "95ebc60d2b1fa672c1f46a8aa265ef51bfe38e7ccb39ec5be34069f1448088435361e70b2ed446e6c9ec387d1d6b3b840f194e373979d241b203c4acafccf5",
            ),
            (
                "462e20412e20486179656b",
                "050e9f3c8fac16b68dbce8f8c4bfbf6617c897f9ada4aa29aa19c8",
            ),
            (
                "4361726c204d656e676572",
                "344233a6cabb7141d80f3da2fedc311d9646bbb0f505afe403a667",
            ),
            (
                "4a65616e2d426170746973746520536179",
                "62cdeeb172ad7ade7aa7d9e069da5790f12331bfa00177787a1d0810c67dc3b2b4",
            ),
            (
                "457567656e2042f6686d20766f6e2042617765726b",
                "029bead1b40992327044d409d9a1f3ad8f36c3c452775d557e18bbeb2e8dfcead32d514024",
            ),
        ],
    };

    /// Noise_IKpsk1_25519_ChaChaPoly_SHA256, from cacophony.
    const IK_PSK1: Vector = Vector {
        prologue: "4a6f686e2047616c74",
        psk: Some("54686973206973206d7920417573747269616e20706572737065637469766521"),
        init_static: "e61ef9919cde45dd5f82166404bd08e38bceb5dfdfded0a34c8df7ed542214d1",
        init_ephemeral: "893e28b9dc6ca8d611ab664754b8ceb7bac5117349a4439a6b0569da977c464a",
        resp_static: "4a3acbfdb163dec651dfa3194dece676d437029c62a408b4c5ea9114246e4893",
        resp_ephemeral: "bbdb4cdbd309f1a1f2e1456967fe288cadd6f712d65dc7b7793d5e63da6b375b",
        handshake_hash: "3ad252ed6f724c52da3450383b7d8b806c183e1ef157bbe0465ad24997ec4717",
        messages: &[
            (
                "4c756477696720766f6e204d69736573",
                "ca35def5ae56cec33dc2036731ab14896bc4c75dbb07a61f879f8e3afa4c7944ac58a6c31948ce200911c5b27b67f1c4d1bed490532dd94ed17164fcc5784d3730fc302b70cc0f19beedaeb56bd974c0e57d747d11534c746eb2a32ac3fde3e4cdf6c3a4705762a6c6ca664b3bc89490",
            ),
            (
                "4d757272617920526f746862617264",
                "95ebc60d2b1fa672c1f46a8aa265ef51bfe38e7ccb39ec5be34069f144808843846a944a0652fb390d213d0700d5ae8fef7aad0ecc79a9216d15de5d7f3ee4",
            ),
            (
                "462e20412e20486179656b",
                "5d872673f64813a47a00369b15c8da92691605ad71ba019de8e718",
            ),
            (
                "4361726c204d656e676572",
                "2045266a750b6af2547f7eb1391058196b742d0aac4b3a1bcc1913",
            ),
            (
                "4a65616e2d426170746973746520536179",
                "9ec47ed0e7628c7d7a4eed631b963740ac2fd754eadaa9232e99054af4f7b29174",
            ),
            (
                "457567656e2042f6686d20766f6e2042617765726b",
                "310e359407350594cfb96eb4596e35677d4a71ceb42aa8cbba097bb9e7150b0d1bd749c4aa",
            ),
        ],
    };
}
