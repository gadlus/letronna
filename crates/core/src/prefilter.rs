//! What each saved session might hold, kept beside its file, so a search of
//! past sessions passes over the ones that cannot match without reading them.
//!
//! Search finds any run of characters, not whole words: "postgres" finds
//! "PostgreSQL", and a run of CJK text has no spaces to cut words at. So what
//! is kept is every three bytes in a row of the lowercased file, hashed into
//! a set of bits: a Bloom filter with one hash. A term with a run of three
//! that is not in the set is surely not in the file; a term whose runs all
//! are may still not be, so a session that passes is always read and the
//! match confirmed against what it says.
//!
//! `sessions/<id>.terms` holds the size and modification time of the session
//! file the set was made from, then the set. It is a cache like `index.json`:
//! one that is missing, torn, from another build or made from the file as it
//! was before it moved is never used, and the next search that reads the
//! file makes it again.

use std::path::{Path, PathBuf};

/// Bumped when how runs are hashed or the file laid out changes, so a set an
/// older build made is made again rather than read wrongly.
const VERSION: u8 = 1;
/// The version, the session file's size and time, and the checksum.
const HEADER: usize = 1 + 8 + 8 + 8;
/// Bits in a set as it is first filled, before it is folded to fit: few runs
/// share a bit at this width, so the bits set count the runs well enough.
const WIDEST: usize = 1 << 20;
/// The fewest bits kept, 128 bytes: the first save of a session holds a
/// message or two.
const NARROWEST: usize = 1 << 10;
/// Bits kept per run a session holds: a run it does not hold then passes
/// about one time in five, and a term of six runs one time in eight thousand.
const BITS_PER_RUN: usize = 4;

/// The runs of three bytes in a session file, as bits.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Filter {
    /// A power of two of bits, and at least `NARROWEST`.
    words: Vec<u64>,
}

impl Filter {
    /// The set for `lower`, the whole of a session file in lowercase, as
    /// narrow as keeps `BITS_PER_RUN` for each run it holds.
    pub(crate) fn of(lower: &str) -> Self {
        let mut filter = Self {
            words: vec![0; WIDEST / 64],
        };
        filter.add(lower);
        let runs = filter.set();
        while filter.bits() / 2 >= NARROWEST.max(runs * BITS_PER_RUN) {
            filter.fold();
        }
        filter
    }

    /// Add the runs in `lower`, text appended to the file in lowercase.
    ///
    /// Each save appends whole lines, and every line ends in a newline that
    /// no term holds (`on_disk` writes it as `\n`), so the runs that cross
    /// from one save into the next are never looked for.
    pub(crate) fn add(&mut self, lower: &str) {
        let mask = self.bits() - 1;
        for hash in runs(lower.as_bytes()) {
            let bit = hash & mask;
            self.words[bit / 64] |= 1 << (bit % 64);
        }
    }

    /// Whether the file may hold `term`, lowercase and as written on disk.
    /// A term shorter than a run has none to rule it out.
    pub(crate) fn may_hold(&self, term: &str) -> bool {
        let mask = self.bits() - 1;
        runs(term.as_bytes()).all(|hash| {
            let bit = hash & mask;
            self.words[bit / 64] & (1 << (bit % 64)) != 0
        })
    }

    /// Whether so many bits are set that a run not in the file passes one
    /// time in three, when it is made again at a width that fits.
    pub(crate) fn crowded(&self) -> bool {
        self.set() * 3 > self.bits()
    }

    fn bits(&self) -> usize {
        self.words.len() * 64
    }

    fn set(&self) -> usize {
        self.words.iter().map(|w| w.count_ones() as usize).sum()
    }

    /// Halve the width. A run's bit is its hash's low bits, so the bit it
    /// had in the upper half is the same bit of the lower half.
    fn fold(&mut self) {
        let half = self.words.len() / 2;
        let (low, high) = self.words.split_at_mut(half);
        for (low, high) in low.iter_mut().zip(high.iter()) {
            *low |= high;
        }
        self.words.truncate(half);
    }
}

/// Each run of three bytes in `bytes`, hashed so that its low bits spread.
fn runs(bytes: &[u8]) -> impl Iterator<Item = usize> + '_ {
    bytes.windows(3).map(|run| {
        let run = u64::from(run[0]) | u64::from(run[1]) << 8 | u64::from(run[2]) << 16;
        // Fibonacci hashing: the top bits of the product mix every input bit.
        (run.wrapping_mul(0x9E37_79B9_7F4A_7C15) >> 40) as usize
    })
}

/// A session file's size and modification time, which a set is only good
/// for while both hold.
pub(crate) type Stamp = (u64, u64);

/// Where the set of the session file at `session` is kept.
pub(crate) fn path_for(session: &Path) -> PathBuf {
    session.with_extension("terms")
}

/// The set of the session file at `session`, if one was made of it as it is
/// at `stamp` and reads back whole.
pub(crate) fn read(session: &Path, stamp: Stamp) -> Option<Filter> {
    let bytes = std::fs::read(path_for(session)).ok()?;
    let (header, body) = bytes.split_at_checked(HEADER)?;
    let number = |at: usize| Some(u64::from_le_bytes(header.get(at..at + 8)?.try_into().ok()?));
    if header[0] != VERSION || (number(1)?, number(9)?) != stamp {
        return None;
    }
    let (words, rest) = body.as_chunks::<8>();
    let words: Vec<u64> = words.iter().map(|word| u64::from_le_bytes(*word)).collect();
    let whole = rest.is_empty()
        && words.len().is_power_of_two()
        && words.len() * 64 >= NARROWEST
        && number(17)? == checksum(stamp, &words);
    whole.then_some(Filter { words })
}

/// Keep `filter` as the set of the session file at `session` as it is at
/// `stamp`. A failure is let go: the next search reads the file instead.
pub(crate) fn write(session: &Path, stamp: Stamp, filter: &Filter) {
    let mut bytes = Vec::with_capacity(HEADER + filter.words.len() * 8);
    bytes.push(VERSION);
    bytes.extend_from_slice(&stamp.0.to_le_bytes());
    bytes.extend_from_slice(&stamp.1.to_le_bytes());
    bytes.extend_from_slice(&checksum(stamp, &filter.words).to_le_bytes());
    for word in &filter.words {
        bytes.extend_from_slice(&word.to_le_bytes());
    }
    // Written in place, not moved into place: two writers at once, or a
    // crash part way, leave a file whose length or checksum is wrong, which
    // is read as no set at all. Private, as the file it sums up is.
    if crate::store::write_private(&path_for(session), &bytes).is_err() {
        let _ = std::fs::remove_file(path_for(session));
    }
}

/// Bring the set of the session file at `session` up to date after a save
/// appended `appended` to it, holding its lock: the file was at `before`
/// until then, and is at `after` now.
///
/// An empty file starts a set. One whose set was made from it as it was
/// before gets the new runs, and is made again from the whole file once
/// they crowd it. Any other is left for a search to make.
pub(crate) fn appended(session: &Path, before: Stamp, after: Stamp, appended: &str) {
    let lower = appended.to_lowercase();
    let mut filter = if before.0 == 0 {
        Filter::of(&lower)
    } else {
        let Some(mut filter) = read(session, before) else {
            return;
        };
        filter.add(&lower);
        filter
    };
    if filter.crowded() {
        let Ok(bytes) = std::fs::read(session) else {
            return;
        };
        filter = Filter::of(&String::from_utf8_lossy(&bytes).to_lowercase());
    }
    write(session, after, &filter);
}

/// A sum of a set and what it was made from, so a file a crash left part
/// written, or two writers mixed, is not taken for a set.
fn checksum(stamp: Stamp, words: &[u64]) -> u64 {
    [stamp.0, stamp.1]
        .iter()
        .chain(words)
        .fold(0x243F_6A88_85A3_08D3, |sum, word| {
            (sum ^ word)
                .wrapping_mul(0x9E37_79B9_7F4A_7C15)
                .rotate_left(29)
        })
}

#[cfg(test)]
mod tests {
    use super::{BITS_PER_RUN, Filter, NARROWEST, appended, path_for, read, write};

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("letronna-prefilter-{name}"));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    const SAID: &str = "{\"type\":\"message\",\"role\":\"user\",\"content\":\"we moved to postgresql — 数据库迁移 \\\"done\\\"\"}\n";

    #[test]
    fn every_run_of_the_text_is_held_whatever_the_width() {
        let lower = SAID.repeat(400).to_lowercase();
        let filter = Filter::of(&lower);
        assert!(filter.bits() >= NARROWEST && filter.bits().is_power_of_two());
        let chars: Vec<(usize, char)> = lower.char_indices().take(SAID.len()).collect();
        for (i, &(start, _)) in chars.iter().enumerate() {
            for &(end, _) in &chars[i + 1..] {
                assert!(
                    filter.may_hold(&lower[start..end]),
                    "{}",
                    &lower[start..end]
                );
            }
        }
    }

    #[test]
    fn a_term_not_there_is_mostly_ruled_out() {
        let filter = Filter::of(&SAID.to_lowercase());
        let absent = ["flamingo", "quetzalcoatl", "mysql", "sqlite", "迁移数据"];
        let passed = absent.iter().filter(|t| filter.may_hold(t)).count();
        assert!(passed <= 1, "{passed} of {} passed", absent.len());
        // Too short to rule out.
        assert!(filter.may_hold("zz"));
    }

    #[test]
    fn it_is_folded_to_a_few_bits_per_run() {
        let small = Filter::of("hello");
        assert_eq!(small.bits(), NARROWEST);
        let big: String = (0..20_000).map(|i| format!("w{i} ")).collect();
        let filter = Filter::of(&big);
        assert!(
            filter.bits() >= filter.set() * BITS_PER_RUN,
            "{}",
            filter.bits()
        );
        assert!(filter.bits() < 1 << 20);
        assert!(!filter.crowded());
    }

    #[test]
    fn a_set_reads_back_only_for_the_file_it_was_made_from() {
        let dir = scratch("stamp");
        let session = dir.join("1.jsonl");
        let filter = Filter::of(&SAID.to_lowercase());
        write(&session, (100, 7), &filter);
        assert_eq!(read(&session, (100, 7)), Some(filter));
        assert_eq!(read(&session, (101, 7)), None);
        assert_eq!(read(&session, (100, 8)), None);
    }

    #[test]
    fn a_torn_or_mixed_set_is_not_used() {
        let dir = scratch("torn");
        let session = dir.join("1.jsonl");
        write(&session, (100, 7), &Filter::of(&SAID.to_lowercase()));
        let whole = std::fs::read(path_for(&session)).unwrap();
        std::fs::write(path_for(&session), &whole[..whole.len() - 8]).unwrap();
        assert_eq!(read(&session, (100, 7)), None);
        // The right length, but a crash left part of it zeros.
        let mut zeroed = whole.clone();
        zeroed[40..].fill(0);
        std::fs::write(path_for(&session), zeroed).unwrap();
        assert_eq!(read(&session, (100, 7)), None);
        std::fs::write(path_for(&session), &whole).unwrap();
        assert!(read(&session, (100, 7)).is_some());
    }

    #[test]
    fn a_save_adds_to_the_set_and_makes_it_again_once_crowded() {
        let dir = scratch("appended");
        let session = dir.join("1.jsonl");
        let mut text = SAID.to_owned();
        std::fs::write(&session, &text).unwrap();
        appended(&session, (0, 0), (1, 1), SAID);
        let first = read(&session, (1, 1)).unwrap();
        for i in 2..200u64 {
            let line = format!("{{\"content\":\"word{i} other{} thing{i}x\"}}\n", i * 7);
            text.push_str(&line);
            std::fs::write(&session, &text).unwrap();
            appended(&session, (i - 1, i - 1), (i, i), &line);
        }
        let last = read(&session, (199, 199)).unwrap();
        assert!(last.bits() > first.bits());
        assert!(!last.crowded());
        let lower = text.to_lowercase();
        assert!(lower.split_whitespace().all(|w| last.may_hold(w)));
        // A save the set was not made before is left for a search.
        appended(&session, (5, 5), (200, 200), "{\"content\":\"late\"}\n");
        assert_eq!(read(&session, (200, 200)), None);
    }
}
