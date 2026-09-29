//! Spoken replies: the answer read aloud a sentence at a time as it streams.
//!
//! Voice mode is off until the user turns it on, because every sentence goes
//! to Letronna's hosted speech model and is paid from their credit. The reply
//! is cut into sentences as it arrives, so the first is heard while the rest
//! are still being written. Code blocks are passed over: nobody wants a
//! regex read to them, and tool output never reaches the splitter at all.
//! One queue fetches the sentences in order and plays them one after
//! another, so two never overlap, and a new turn or Stop empties it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

/// Words that end in a full stop without ending the sentence.
const ABBREVIATIONS: &[&str] = &[
    "e.g", "i.e", "etc", "vs", "mr", "mrs", "ms", "dr", "st", "jr", "sr", "no", "fig", "cf",
    "approx", "inc", "ltd",
];
/// How often a playing sentence looks to see whether it was stopped.
const POLL: Duration = Duration::from_millis(50);
/// The most a voice note sent to a chat reads aloud; the hosted model takes
/// about 4,000 characters in one request, and nobody listens to more.
const NOTE_CHARACTERS: usize = 4000;

/// Cuts streamed reply text into sentences worth saying.
#[derive(Default)]
pub struct Sentences {
    /// Text heard but not yet cut off as a sentence.
    pending: String,
    /// Inside a fenced code block, which is not read aloud.
    fenced: bool,
    /// `pending` starts partway through a line, where no fence can open.
    mid_line: bool,
    /// The line under way is a table row, which is not read aloud either.
    row: bool,
}

impl Sentences {
    /// Take in the next piece of the reply and hand back the sentences it finished.
    pub fn push(&mut self, text: &str) -> Vec<String> {
        self.pending.push_str(text);
        self.cut(false)
    }

    /// End the reply, handing back whatever was left as its last sentence.
    pub fn finish(&mut self) -> Vec<String> {
        let said = self.cut(true);
        *self = Sentences::default();
        said
    }

    fn cut(&mut self, done: bool) -> Vec<String> {
        let mut said = Vec::new();
        while !self.pending.is_empty() {
            let line_end = self.pending.find('\n');
            if !self.mid_line {
                let start = self.pending.trim_start_matches([' ', '\t']);
                if is_fence(start) {
                    // The whole fence line is needed: its language tag is
                    // not something to read aloud either.
                    let Some(end) = line_end else {
                        if done {
                            self.pending.clear();
                        }
                        break;
                    };
                    self.pending.drain(..=end);
                    self.fenced = !self.fenced;
                    continue;
                }
                // Too little of the line has come to know whether it opens one.
                if line_end.is_none() && !done && could_be_fence(start) {
                    break;
                }
                // Decided at the row's start, since a stop inside a cell
                // would otherwise let the rest of the row be said.
                self.row = !self.fenced && start.starts_with('|');
            }
            if self.fenced || self.row {
                match line_end {
                    Some(end) => {
                        self.pending.drain(..=end);
                        self.mid_line = false;
                        self.row = false;
                    }
                    None => {
                        self.pending.clear();
                        self.mid_line = true;
                    }
                }
                continue;
            }
            let Some(end) = sentence_end(&self.pending, line_end, done) else {
                break;
            };
            let piece: String = self.pending.drain(..end).collect();
            let rest = self.pending.trim_start_matches([' ', '\t']).len();
            self.pending.drain(..self.pending.len() - rest);
            self.mid_line = true;
            if self.pending.starts_with('\n') {
                self.pending.drain(..1);
                self.mid_line = false;
            }
            let spoken = clean(&piece);
            if !spoken.is_empty() {
                said.push(spoken);
            }
        }
        said
    }
}

/// Whether a line opens or closes a fenced code block.
fn is_fence(line: &str) -> bool {
    line.starts_with("```") || line.starts_with("~~~")
}

/// Whether a line this short may still turn out to be a fence.
fn could_be_fence(line: &str) -> bool {
    line.len() < 3 && ("```".starts_with(line) || "~~~".starts_with(line))
}

/// Where the first sentence in `text` ends, if it can be told yet: after the
/// punctuation that ends it, or at the end of its line.
fn sentence_end(text: &str, line_end: Option<usize>, done: bool) -> Option<usize> {
    let line = &text[..line_end.unwrap_or(text.len())];
    for (at, mark) in line.char_indices() {
        if !matches!(mark, '.' | '!' | '?') {
            continue;
        }
        let after = at + mark.len_utf8();
        match text[after..].chars().next() {
            // What follows decides it, and it has not come yet.
            None if !done => return None,
            None => return Some(after),
            Some(next) if next.is_whitespace() => {
                if mark == '.' && abbreviated(&line[..at]) {
                    continue;
                }
                return Some(after);
            }
            // A decimal, a URL or a file name: the stop is inside a word.
            Some(_) => {}
        }
    }
    match line_end {
        Some(end) => Some(end),
        None if done => Some(text.len()),
        None => None,
    }
}

/// Whether the word before a full stop makes it part of the sentence: an
/// abbreviation, an initial, or the number of a numbered list.
fn abbreviated(before: &str) -> bool {
    let word = before
        .rsplit(|c: char| c.is_whitespace() || c == '(')
        .next()
        .unwrap_or_default();
    let lower = word.to_lowercase();
    if ABBREVIATIONS.contains(&lower.as_str()) {
        return true;
    }
    // Letters with stops between them, as in U.S. or a.m., but not a file
    // name or an address, whose stop at the end does end the sentence.
    let dotted = word.contains('.')
        && word
            .split('.')
            .all(|part| (1..=2).contains(&part.len()) && part.chars().all(char::is_alphabetic));
    if dotted {
        return true;
    }
    if word.chars().count() == 1 && word.chars().all(char::is_alphabetic) {
        return true;
    }
    let first = before.trim_start().len() == word.len();
    first && !word.is_empty() && word.chars().all(|c| c.is_ascii_digit())
}

/// A sentence as it should be said: without the Markdown that marks it up,
/// with a link's words rather than its address, and empty when nothing in
/// it could be said, such as a table's rules.
fn clean(piece: &str) -> String {
    let mut text = piece.trim();
    if text.starts_with('|') {
        return String::new();
    }
    text = text.trim_start_matches(['#', '>', ' ']);
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = text.strip_prefix(bullet) {
            text = rest;
        }
    }
    let text = unaddressed(&unlinked(text));
    let text: String = text.chars().filter(|c| !matches!(c, '*' | '`')).collect();
    let text = text.replace("~~", "");
    if !text.chars().any(char::is_alphanumeric) {
        return String::new();
    }
    text.trim().to_owned()
}

/// `text` with each Markdown link as its words alone.
fn unlinked(text: &str) -> String {
    let mut out = String::new();
    let mut rest = text;
    while let Some(open) = rest.find('[') {
        let inside = &rest[open + 1..];
        let Some(close) = inside.find("](") else {
            break;
        };
        let Some(end) = inside[close + 2..].find(')') else {
            break;
        };
        out.push_str(&rest[..open]);
        out.push_str(&inside[..close]);
        rest = &inside[close + 2 + end + 1..];
    }
    out.push_str(rest);
    out
}

/// `text` with each web address said as "the link": one read out a
/// character at a time helps nobody.
fn unaddressed(text: &str) -> String {
    let words: Vec<String> = text
        .split(' ')
        .map(|word| {
            if !(word.starts_with("http://") || word.starts_with("https://")) {
                return word.to_owned();
            }
            let address = word.trim_end_matches(['.', ',', ';', ':', '!', '?', ')']);
            format!("the link{}", &word[address.len()..])
        })
        .collect();
    words.join(" ")
}

/// A whole reply as the words a voice note says, capped at what one request
/// to the speech model takes.
pub fn words(reply: &str) -> String {
    let mut sentences = Sentences::default();
    let mut all = sentences.push(reply);
    all.extend(sentences.finish());
    let mut said = String::new();
    for sentence in all {
        if said.chars().count() + sentence.chars().count() + 1 > NOTE_CHARACTERS {
            // A first sentence longer than one request takes, such as a long
            // line with no stop in it, is said as far as it fits rather
            // than leaving nothing to say.
            if said.is_empty() {
                said = sentence.chars().take(NOTE_CHARACTERS).collect();
            }
            break;
        }
        if !said.is_empty() {
            said.push(' ');
        }
        said.push_str(&sentence);
    }
    said
}

/// Read `reply` aloud as a voice note for a chat app, saved in Letronna's own
/// `media/` for the chat to be sent.
pub fn voice_note(reply: &str) -> Result<PathBuf, String> {
    let said = words(reply);
    if said.is_empty() {
        return Err("The reply had nothing to read aloud.".to_owned());
    }
    // Opus in Ogg is what Telegram shows as a voice note rather than a track.
    let (bytes, extension) = crate::media::speech(&said, "opus")?;
    crate::media::save(
        &crate::tools::ToolContext::default(),
        &said,
        extension,
        &bytes,
    )
}

/// The sound that came back for one sentence.
pub struct Audio {
    pub bytes: Vec<u8>,
    /// The file type it came as, such as wav.
    pub extension: &'static str,
}

/// Somewhere speech is played: the window's sound device or the computer's own player.
pub trait Output {
    /// Play `audio` to its end, or until `stopped` says to stop.
    fn play(&mut self, audio: &Audio, stopped: &dyn Fn() -> bool) -> Result<(), String>;
}

/// Opens the output on the thread that plays, since a sound device handle
/// cannot be sent between threads on every platform.
pub type Open = Box<dyn FnOnce() -> Result<Box<dyn Output>, String> + Send>;

type Speak = Arc<dyn Fn(&str) -> Result<Audio, String> + Send + Sync>;

/// Voice mode while it is on: the splitter and the queue that says what it cuts.
pub struct Voice {
    sentences: Sentences,
    queue: mpsc::Sender<(u64, String)>,
    /// Moved on by `hush`; anything queued under an earlier number is dropped.
    turn: Arc<AtomicU64>,
    /// The first thing that went wrong, held until the front end shows it.
    trouble: Arc<Mutex<Option<String>>>,
}

impl Voice {
    /// Turn voice mode on, speaking through the hosted model and playing on `open`.
    pub fn start(open: Open) -> Voice {
        let speak: Speak = Arc::new(|text: &str| {
            // WAV, because it is what every player here can take, the
            // Windows SoundPlayer and aplay among them.
            let (bytes, extension) = crate::media::speech(text, "wav")?;
            Ok(Audio { bytes, extension })
        });
        Voice::with(speak, open)
    }

    fn with(speak: Speak, open: Open) -> Voice {
        let turn = Arc::new(AtomicU64::new(0));
        let trouble = Arc::new(Mutex::new(None));
        let (queue, sentences) = mpsc::channel::<(u64, String)>();
        // One sentence fetched ahead of the one playing: enough that there
        // is no gap between them, and little paid for when the rest is stopped.
        let (fetched, heard) = mpsc::sync_channel::<(u64, Audio)>(1);
        let (now, fail, failed) = (turn.clone(), trouble.clone(), Arc::new(AtomicU64::new(0)));
        std::thread::spawn(move || {
            for (at, text) in sentences {
                let dropped = failed.load(Ordering::SeqCst) == at + 1;
                if at != now.load(Ordering::SeqCst) || dropped {
                    continue;
                }
                match speak(&text) {
                    Ok(audio) => {
                        if fetched.send((at, audio)).is_err() {
                            return;
                        }
                    }
                    Err(why) => {
                        // The rest of this reply would only fail the same way,
                        // and each try may cost.
                        failed.store(at + 1, Ordering::SeqCst);
                        keep(&fail, why);
                    }
                }
            }
        });
        let (now, fail) = (turn.clone(), trouble.clone());
        std::thread::spawn(move || {
            let mut output = match open() {
                Ok(output) => output,
                Err(why) => return keep(&fail, why),
            };
            for (at, audio) in heard {
                if at != now.load(Ordering::SeqCst) {
                    continue;
                }
                let stopped = || at != now.load(Ordering::SeqCst);
                if let Err(why) = output.play(&audio, &stopped) {
                    keep(&fail, why);
                }
            }
        });
        Voice {
            sentences: Sentences::default(),
            queue,
            turn,
            trouble,
        }
    }

    /// Take in the next piece of the reply, queueing each sentence it finishes.
    pub fn hear(&mut self, text: &str) {
        let said = self.sentences.push(text);
        self.say(said);
    }

    /// The reply is over: queue what was left of it.
    pub fn end(&mut self) {
        let said = self.sentences.finish();
        self.say(said);
    }

    /// Stop speaking, dropping every sentence not yet heard.
    pub fn hush(&mut self) {
        self.turn.fetch_add(1, Ordering::SeqCst);
        self.sentences = Sentences::default();
    }

    /// What went wrong since this was last asked, if anything did.
    pub fn trouble(&self) -> Option<String> {
        self.trouble.lock().ok().and_then(|mut held| held.take())
    }

    fn say(&self, said: Vec<String>) {
        let at = self.turn.load(Ordering::SeqCst);
        for sentence in said {
            // A queue whose threads have gone has already said why.
            let _ = self.queue.send((at, sentence));
        }
    }
}

impl Drop for Voice {
    fn drop(&mut self) {
        // Stops the sentence playing now; the threads end once the queue goes.
        self.hush();
    }
}

fn keep(trouble: &Mutex<Option<String>>, why: String) {
    if let Ok(mut held) = trouble.lock() {
        held.get_or_insert(why);
    }
}

/// Plays through the computer's own player, one sentence at a time.
pub struct System;

impl Output for System {
    fn play(&mut self, audio: &Audio, stopped: &dyn Fn() -> bool) -> Result<(), String> {
        let path = spool(&std::env::temp_dir(), audio)?;
        let played = play_file(&path, stopped);
        let _ = std::fs::remove_file(&path);
        played
    }
}

/// Write `audio` to a file of its own in `dir` for a player to open.
fn spool(dir: &Path, audio: &Audio) -> Result<PathBuf, String> {
    use std::io::Write;
    // On Linux the temp folder is shared by every user and the name can be
    // guessed, so a file only this call created is written: a plain write
    // would follow a link planted there and overwrite what it points to.
    for n in 0..100 {
        let path = dir.join(format!(
            "letronna-speech-{}-{n}.{}",
            std::process::id(),
            audio.extension
        ));
        let created = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path);
        match created {
            Ok(mut file) => {
                file.write_all(&audio.bytes)
                    .map_err(|e| format!("Cannot keep the speech to play it: {e}"))?;
                return Ok(path);
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("Cannot keep the speech to play it: {e}")),
        }
    }
    Err("Cannot keep the speech to play it: the temp folder is full of old ones.".to_owned())
}

fn play_file(path: &Path, stopped: &dyn Fn() -> bool) -> Result<(), String> {
    let mut child = players(path)
        .into_iter()
        .find_map(|mut player| {
            player
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .ok()
        })
        .ok_or_else(|| "No sound player was found to read the reply aloud.".to_owned())?;
    loop {
        if stopped() {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(());
        }
        match child.try_wait() {
            Ok(Some(_)) => return Ok(()),
            Ok(None) => std::thread::sleep(POLL),
            Err(e) => return Err(format!("The sound player stopped answering: {e}")),
        }
    }
}

/// The players to try for `path`, the first that starts being the one used.
#[cfg(target_os = "macos")]
fn players(path: &Path) -> Vec<Command> {
    let mut afplay = Command::new("afplay");
    afplay.arg(path);
    vec![afplay]
}

/// The players to try for `path`, the first that starts being the one used.
#[cfg(windows)]
fn players(path: &Path) -> Vec<Command> {
    let quoted = path.display().to_string().replace('\'', "''");
    let mut shell = Command::new("powershell");
    shell.args([
        "-NoProfile",
        "-NonInteractive",
        "-Command",
        &format!("(New-Object Media.SoundPlayer '{quoted}').PlaySync()"),
    ]);
    vec![shell]
}

/// The players to try for `path`, the first that starts being the one used:
/// PulseAudio's, which PipeWire answers too, then bare ALSA.
#[cfg(not(any(target_os = "macos", windows)))]
fn players(path: &Path) -> Vec<Command> {
    ["paplay", "aplay"]
        .into_iter()
        .map(|name| {
            let mut player = Command::new(name);
            player.arg(path);
            player
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    fn all(pieces: &[&str]) -> Vec<String> {
        let mut sentences = Sentences::default();
        let mut said: Vec<String> = pieces.iter().flat_map(|p| sentences.push(p)).collect();
        said.extend(sentences.finish());
        said
    }

    #[test]
    fn sentences_are_cut_at_their_ends_and_not_at_abbreviations() {
        let said = all(&["Use a flag, e.g. --force. Mr. Smith agreed! Did J. R. R. Tolkien? Yes."]);
        assert_eq!(
            said,
            [
                "Use a flag, e.g. --force.",
                "Mr. Smith agreed!",
                "Did J. R. R. Tolkien?",
                "Yes."
            ]
        );
    }

    #[test]
    fn numbers_urls_and_file_names_do_not_end_a_sentence() {
        let said = all(&[
            "Pi is 3.14 and v1.2.3 shipped. See https://example.com/a.b?x=1 for more. ",
            "Edit main.rs next. It is in the U.S. office.\n1. First step\n2. Second step",
        ]);
        assert_eq!(
            said,
            [
                "Pi is 3.14 and v1.2.3 shipped.",
                "See the link for more.",
                "Edit main.rs next.",
                "It is in the U.S. office.",
                "1. First step",
                "2. Second step"
            ]
        );
    }

    #[test]
    fn a_code_block_is_passed_over() {
        let said = all(&[
            "Run this:\n```bash\necho \"Hi. There.\"\n",
            "ls -la\n```\nThen **check** the `output`.",
        ]);
        assert_eq!(said, ["Run this:", "Then check the output."]);
    }

    #[test]
    fn a_partial_sentence_waits_for_the_rest_and_is_said_at_the_end() {
        let mut sentences = Sentences::default();
        assert_eq!(sentences.push("The answer is 4"), Vec::<String>::new());
        assert_eq!(sentences.push("2."), Vec::<String>::new());
        assert_eq!(sentences.push(" And then"), ["The answer is 42."]);
        assert_eq!(sentences.push(" some"), Vec::<String>::new());
        assert_eq!(sentences.finish(), ["And then some"]);
        assert_eq!(sentences.finish(), Vec::<String>::new());
    }

    #[test]
    fn text_streamed_a_character_at_a_time_is_cut_as_it_would_be_whole() {
        let reply = "Hello there. Here is code:\n```rust\nlet x = 1.5;\n```\n- A [link](https://x.io). Done";
        let pieces: Vec<String> = reply.chars().map(String::from).collect();
        let pieces: Vec<&str> = pieces.iter().map(String::as_str).collect();
        assert_eq!(all(&pieces), all(&[reply]));
        assert_eq!(
            all(&[reply]),
            ["Hello there.", "Here is code:", "A link.", "Done"]
        );
    }

    #[test]
    fn a_voice_note_says_the_reply_without_its_code() {
        assert_eq!(
            words("# Result\nIt worked.\n```\nrm -rf x\n```\n| a | b |\nBye."),
            "Result It worked. Bye."
        );
    }

    #[test]
    fn a_table_row_is_passed_over_even_with_a_stop_inside_it() {
        let reply = "| Step | Run it. Then check it |\n|---|---|\nDone.";
        let pieces: Vec<String> = reply.chars().map(String::from).collect();
        let pieces: Vec<&str> = pieces.iter().map(String::as_str).collect();
        assert_eq!(all(&[reply]), ["Done."]);
        assert_eq!(all(&pieces), ["Done."]);
    }

    #[test]
    fn a_voice_note_of_one_long_line_says_its_start() {
        let long = "word ".repeat(2000);
        let said = words(&long);
        assert!(!said.is_empty());
        assert!(said.chars().count() <= NOTE_CHARACTERS);
    }

    #[cfg(unix)]
    #[test]
    fn speech_kept_for_the_player_does_not_follow_a_planted_link() {
        let dir = std::env::temp_dir().join(format!("letronna-spool-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let victim = dir.join("victim");
        std::fs::write(&victim, "keep me").unwrap();
        let planted = dir.join(format!("letronna-speech-{}-0.wav", std::process::id()));
        std::os::unix::fs::symlink(&victim, &planted).unwrap();
        let audio = Audio {
            bytes: b"sound".to_vec(),
            extension: "wav",
        };
        let path = spool(&dir, &audio).unwrap();
        assert_ne!(path, planted);
        assert_eq!(std::fs::read(&path).unwrap(), b"sound");
        assert_eq!(std::fs::read_to_string(&victim).unwrap(), "keep me");
        std::fs::remove_dir_all(dir).unwrap();
    }

    /// An output that holds the first sentence until it is stopped, and logs
    /// what it played and whether it was cut off.
    struct Recorder {
        log: Arc<Mutex<Vec<String>>>,
    }

    impl Output for Recorder {
        fn play(&mut self, audio: &Audio, stopped: &dyn Fn() -> bool) -> Result<(), String> {
            let text = String::from_utf8_lossy(&audio.bytes).into_owned();
            let first = self.log.lock().map(|log| log.is_empty()).unwrap_or(false);
            self.log.lock().unwrap().push(text);
            let started = Instant::now();
            while first && !stopped() && started.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(5));
            }
            if first && stopped() {
                self.log.lock().unwrap().push("(stopped)".to_owned());
            }
            Ok(())
        }
    }

    fn wait_for(log: &Mutex<Vec<String>>, what: &str) -> Vec<String> {
        let started = Instant::now();
        while started.elapsed() < Duration::from_secs(5) {
            let now = log.lock().unwrap().clone();
            if now.iter().any(|line| line == what) {
                return now;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        log.lock().unwrap().clone()
    }

    fn recording() -> (Voice, Arc<Mutex<Vec<String>>>) {
        let log = Arc::new(Mutex::new(Vec::new()));
        let recorder = Recorder { log: log.clone() };
        let speak: Speak = Arc::new(|text: &str| {
            Ok(Audio {
                bytes: text.as_bytes().to_vec(),
                extension: "wav",
            })
        });
        (
            Voice::with(speak, Box::new(move || Ok(Box::new(recorder)))),
            log,
        )
    }

    #[test]
    fn hushing_stops_the_sentence_playing_and_drops_the_rest() {
        let (mut voice, log) = recording();
        voice.hear("One. Two. Three. Four");
        wait_for(&log, "One.");
        voice.hush();
        voice.hear("Next turn.");
        voice.end();
        let heard = wait_for(&log, "Next turn.");
        assert_eq!(heard, ["One.", "(stopped)", "Next turn."]);
    }

    #[test]
    fn a_failed_sentence_is_reported_and_the_rest_of_its_reply_dropped() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let recorder = Recorder { log: log.clone() };
        let speak: Speak = Arc::new(|text: &str| match text {
            "Paid." => Err("Out of credit.".to_owned()),
            _ => Ok(Audio {
                bytes: text.as_bytes().to_vec(),
                extension: "wav",
            }),
        });
        let mut voice = Voice::with(speak, Box::new(move || Ok(Box::new(recorder))));
        voice.hear("Paid. Never fetched. ");
        let started = Instant::now();
        let mut trouble = None;
        while trouble.is_none() && started.elapsed() < Duration::from_secs(5) {
            trouble = voice.trouble();
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(trouble.as_deref(), Some("Out of credit."));
        voice.hush();
        voice.hear("Later.");
        voice.end();
        assert_eq!(wait_for(&log, "Later."), ["Later."]);
    }
}
