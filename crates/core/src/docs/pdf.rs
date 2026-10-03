//! PDF files as read_file shows them: each page's text under `## Page N of
//! M`, as pdf-extract finds it, and a note in place of pages with no text to
//! read, such as scans, which name a way to read them as pictures.
//!
//! The parser is someone else's and a PDF is a small program, so besides the
//! thread and deadline every document gets, a file is read within limits of
//! its own: its size; what each of its streams unpacks to, measured before
//! the parser unpacks it; and what a page draws -- drawings within drawings,
//! none drawing itself, which would run the parser out of stack and end
//! Letronna -- checked before the page is read. A panic in the parser costs
//! only its page.

use super::{DEADLINE, FAILED, View, late};
use pdf_extract::{
    Dictionary, Document, LoadOptions, MediaBox, Object, ObjectId, OutputDev, OutputError,
    PlainTextOutput, Stream, Transform,
};
use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

/// The largest PDF read, in bytes: a scanned book is a few hundred MB, where
/// a long paper or manual with its text is a few.
const FILE_BYTES: u64 = 50 * 1024 * 1024;
/// The most a stream other than a picture may unpack to. A page's drawing
/// and a whole font are a few MB; a stream made to fill the memory says far
/// more, and is refused before the parser unpacks it.
const STREAM_BYTES: u64 = 64 * 1024 * 1024;
/// The most the packed objects met while loading may unpack to together.
const PACKED_BYTES: u64 = 256 * 1024 * 1024;
/// The most a page may unpack to be read: its drawing and those it draws,
/// each as often as it draws them.
const PAGE_BYTES: u64 = 128 * 1024 * 1024;
/// The most drawings a page may draw, those within others counted.
const DRAWS: usize = 2_000;
/// How deep drawings may sit within each other.
const DEPTH: usize = 32;
/// How long before the call stops waiting the reader stops starting pages,
/// so the pages it read come back.
const MARGIN: Duration = Duration::from_secs(2);
/// How a page with no text to read is named: "Pages 2-5 have" ...
const SCANNED: &str = "no text layer (scanned); read_file cannot read them. Render them to PNG \
                       with a tool such as pdftoppm, then read_file the pictures.";

thread_local! {
    /// What the load filter, a plain function, has met: when to stop and
    /// how much the packed objects unpacked to, or past which limit.
    static LOAD: Cell<Load> = const {
        Cell::new(Load {
            until: None,
            packed: 0,
            over: false,
        })
    };
}

#[derive(Clone, Copy)]
struct Load {
    until: Option<Instant>,
    packed: u64,
    over: bool,
}

/// The view of the PDF at `path`, read until `until`.
pub(super) fn read(path: &Path, until: Instant) -> Result<String, String> {
    let too_big = || {
        format!(
            "it is over {} MB, more than read_file reads; `pdftotext` reads one that size",
            FILE_BYTES / (1024 * 1024)
        )
    };
    let file = File::open(path).map_err(|e| e.to_string())?;
    if file.metadata().is_ok_and(|meta| meta.len() > FILE_BYTES) {
        return Err(too_big());
    }
    // Read no further than the limit, should it have grown since.
    let mut bytes = Vec::new();
    file.take(FILE_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| e.to_string())?;
    if bytes.len() as u64 > FILE_BYTES {
        return Err(too_big());
    }
    let soft = until.checked_sub(MARGIN).unwrap_or(until);
    // Unpacked by the parser as it loads, before any filter sees them.
    if let Some(over) = xref_streams_over(&bytes, soft) {
        return Err(over);
    }
    let mut doc = load(&bytes, soft)?;
    if doc.trailer.get(b"Encrypt").is_ok() {
        return Err(
            "it is locked with a password, which read_file cannot give. With the password, \
             `qpdf --password=<it> --decrypt in.pdf out.pdf` makes a copy it can read"
                .into(),
        );
    }
    let (pictures, sizes) = prepare(&mut doc, soft)?;
    let pages = doc.get_pages();
    if pages.is_empty() {
        return Err("it has no pages that can be found; `pdftotext` may still read it".into());
    }
    let total = pages.len();
    let mut read = Vec::with_capacity(total);
    let mut names = HashMap::new();
    for (&n, &id) in &pages {
        if Instant::now() >= soft {
            break;
        }
        let page = Page {
            doc: &doc,
            sizes: &sizes,
            pictures: &pictures,
        };
        let shown = match page.drawn(id, &mut names) {
            Ok(drawn) => text_of(&doc, n, soft)
                .map_or_else(Shown::Failed, |text| shown(text, drawn.pictured)),
            Err(why) => Shown::Failed(why.into()),
        };
        read.push(shown);
    }
    Ok(assemble(&read, total))
}

/// The document in `bytes`, loaded with its packed objects measured as they
/// are met, until `until`.
fn load(bytes: &[u8], until: Instant) -> Result<Document, String> {
    LOAD.set(Load {
        until: Some(until),
        packed: 0,
        over: false,
    });
    let options = LoadOptions {
        filter: Some(admit),
        ..LoadOptions::default()
    };
    let loaded = crate::diag::contained(|| Document::load_mem_with_options(bytes, options));
    let met = LOAD.replace(Load {
        until: None,
        packed: 0,
        over: false,
    });
    if met.over {
        return Err(over());
    }
    if Instant::now() >= until {
        return Err(late());
    }
    match loaded {
        Ok(Ok(doc)) => Ok(doc),
        Ok(Err(e)) if contains(bytes, b"/Encrypt") => {
            Err(format!("it is encrypted, or damaged ({e})"))
        }
        Ok(Err(e)) => Err(format!(
            "it is damaged or cut short ({e}); `pdftotext` may still read it"
        )),
        Err(_) => Err(FAILED.into()),
    }
}

/// The load filter: a packed set of objects is unpacked as it is loaded, so
/// it is measured first, and dropped once the packed ones together, or one
/// alone, unpack past their limit, or the time is up.
fn admit(id: ObjectId, object: &mut Object) -> Option<(ObjectId, Object)> {
    let Object::Stream(stream) = object else {
        // An object packed with others is kept as given back here; any other
        // as it was left.
        return Some((id, object.clone()));
    };
    if !stream.dict.has_type(b"ObjStm") {
        return Some((id, Object::Null));
    }
    let mut met = LOAD.get();
    if met.over || met.until.is_some_and(|until| Instant::now() >= until) {
        return None;
    }
    match unpacked(stream, STREAM_BYTES) {
        Some(size) if met.packed + size <= PACKED_BYTES => met.packed += size,
        _ => met.over = true,
    }
    LOAD.set(met);
    (!met.over).then_some((id, Object::Null))
}

/// Why the file is not read when one of its streams unpacks past a limit.
fn over() -> String {
    format!(
        "a part of it unpacks to more than {} MB, more than any page's text needs",
        STREAM_BYTES / (1024 * 1024)
    )
}

/// Pictures emptied, as text is all that is read and the parser would read
/// their pixels as drawing; and every other stream measured, by what it
/// unpacks to. Gives the pictures and the sizes.
fn prepare(
    doc: &mut Document,
    until: Instant,
) -> Result<(HashSet<ObjectId>, HashMap<ObjectId, u64>), String> {
    let (mut pictures, mut sizes) = (HashSet::new(), HashMap::new());
    for (&id, object) in &mut doc.objects {
        if Instant::now() >= until {
            return Err(late());
        }
        let Object::Stream(stream) = object else {
            continue;
        };
        let named = |key: &[u8], name: &[u8]| {
            stream
                .dict
                .get(key)
                .and_then(Object::as_name)
                .is_ok_and(|n| n == name)
        };
        let picture = named(b"Subtype", b"Image");
        let unread = named(b"Type", b"EmbeddedFile") || named(b"Type", b"Metadata");
        if picture {
            pictures.insert(id);
        }
        if picture || unread {
            stream.set_plain_content(Vec::new());
        } else {
            let size = unpacked(stream, STREAM_BYTES).ok_or_else(over)?;
            sizes.insert(id, size);
        }
    }
    Ok((pictures, sizes))
}

/// What `stream` unpacks to as the parser unpacks it, or `None` past `most`
/// bytes, found without ever holding more than that.
fn unpacked(stream: &Stream, most: u64) -> Option<u64> {
    let Ok(filters) = stream.filters() else {
        let size = stream.content.len() as u64;
        return (size <= most).then_some(size);
    };
    let early = stream
        .dict
        .get(b"DecodeParms")
        .and_then(Object::as_dict)
        .and_then(|params| params.get(b"EarlyChange"))
        .and_then(Object::as_i64)
        .ok()
        .is_none_or(|early| early != 0);
    let mut data = std::borrow::Cow::Borrowed(stream.content.as_slice());
    for filter in filters {
        data = std::borrow::Cow::Owned(match filter {
            b"FlateDecode" => inflate(&data, most)?,
            b"LZWDecode" => unlzw(&data, early, most)?,
            // At most four bytes for each one in.
            b"ASCII85Decode" => {
                let mut once = Stream::new(Dictionary::new(), data.into_owned());
                once.dict.set("Filter", "ASCII85Decode");
                once.decompressed_content().ok()?
            }
            // The parser stops at a filter it lacks, and the stream stays as
            // it is.
            _ => break,
        });
    }
    let size = data.len() as u64;
    (size <= most).then_some(size)
}

/// `input` inflated, as the parser inflates it, or `None` past `most` bytes.
fn inflate(input: &[u8], most: u64) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let zlib = flate2::read::ZlibDecoder::new(input)
        .take(most + 1)
        .read_to_end(&mut out);
    // A stream whose check fails is taken as plain deflate, as the parser
    // does.
    if zlib.is_err() && out.is_empty() && input.len() > 2 {
        let _ = flate2::read::DeflateDecoder::new(&input[2..])
            .take(most + 1)
            .read_to_end(&mut out);
    }
    (out.len() as u64 <= most).then_some(out)
}

/// `input` unpacked from LZW, as the parser unpacks it, or `None` past
/// `most` bytes.
fn unlzw(input: &[u8], early: bool, most: u64) -> Option<Vec<u8>> {
    use weezl::{BitOrder, LzwStatus, decode::Decoder};
    let mut decoder = match early {
        true => Decoder::with_tiff_size_switch(BitOrder::Msb, 8),
        false => Decoder::new(BitOrder::Msb, 8),
    };
    let (mut out, mut chunk, mut rest) = (Vec::new(), vec![0; 64 * 1024], input);
    loop {
        let step = decoder.decode_bytes(rest, &mut chunk);
        out.extend_from_slice(&chunk[..step.consumed_out]);
        rest = &rest[step.consumed_in..];
        if out.len() as u64 > most {
            return None;
        }
        match step.status {
            Ok(LzwStatus::Ok) if step.consumed_in + step.consumed_out > 0 => {}
            _ => return Some(out),
        }
    }
}

/// Why `bytes` is not read when the tables of where its objects lie are
/// themselves packed and unpack past `STREAM_BYTES`: the parser unpacks them
/// as the file opens, before anything else can be measured.
fn xref_streams_over(bytes: &[u8], until: Instant) -> Option<String> {
    // Each table's dictionary names its type, then the word stream and a
    // line end open its data; the next such word is found again only once
    // passed, so the file is gone through once.
    let (mut at, mut next, mut checked) = (0, None::<usize>, None);
    while let Some(found) = find(&bytes[at..], b"/XRef") {
        if Instant::now() >= until {
            return Some(late());
        }
        at += found + b"/XRef".len();
        if next.is_none_or(|open| open < at) {
            next = find(&bytes[at..], b"stream").map(|i| at + i);
        }
        let open = next?;
        if checked == Some(open) {
            continue;
        }
        checked = Some(open);
        let start = open + b"stream".len();
        let start = start
            + match bytes.get(start..start + 2) {
                Some(b"\r\n") => 2,
                Some([b'\n' | b'\r', _]) => 1,
                _ => continue,
            };
        // Inflating stops where its data ends.
        if inflate(&bytes[start..], STREAM_BYTES).is_none() {
            return Some(over());
        }
    }
    None
}

/// Where `needle` first is in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    find(haystack, needle).is_some()
}

/// A page and what was learned of the document before reading it.
struct Page<'a> {
    doc: &'a Document,
    sizes: &'a HashMap<ObjectId, u64>,
    pictures: &'a HashSet<ObjectId>,
}

/// What a page draws, as checked before it is read.
#[derive(Default)]
struct Drawn {
    draws: usize,
    bytes: u64,
    pictured: bool,
}

impl Page<'_> {
    /// What page `id` draws, or why it is not safe to read: the parser
    /// follows its parents for what it inherits and the drawings it draws
    /// without counting either, so a loop in one, a drawing drawing itself,
    /// or a page that unpacks past `PAGE_BYTES` would never end. `names`
    /// keeps the drawings each stream names, read once.
    fn drawn(
        &self,
        id: ObjectId,
        names: &mut HashMap<ObjectId, Vec<Vec<u8>>>,
    ) -> Result<Drawn, &'static str> {
        let doc = self.doc;
        let page = doc.get_dictionary(id).map_err(|_| "it is not a page")?;
        let mut seen = HashSet::from([id]);
        let mut node = page;
        while let Ok(parent) = node.get(b"Parent").and_then(Object::as_reference) {
            if !seen.insert(parent) {
                return Err("its place in the list of pages loops");
            }
            let Ok(next) = doc.get_dictionary(parent) else {
                break;
            };
            node = next;
        }
        let mut drawn = Drawn::default();
        for content in doc.get_page_contents(id) {
            drawn.bytes += self.sizes.get(&content).copied().unwrap_or_default();
        }
        if drawn.bytes > PAGE_BYTES {
            return Err(too_much());
        }
        let resources = inherited(doc, page, b"Resources");
        let content = doc.get_page_content(id).unwrap_or_default();
        let (named, inline) = drawings(&content);
        drawn.pictured |= inline;
        let mut path = Vec::new();
        self.draw(&named, resources, &mut path, &mut drawn, names)?;
        Ok(drawn)
    }

    /// Follows the drawings named in `named` from `resources`, as the parser
    /// will, within the limits.
    fn draw(
        &self,
        named: &[Vec<u8>],
        resources: Option<&Dictionary>,
        path: &mut Vec<ObjectId>,
        drawn: &mut Drawn,
        names: &mut HashMap<ObjectId, Vec<Vec<u8>>>,
    ) -> Result<(), &'static str> {
        let doc = self.doc;
        let Some(xobjects) = resources
            .and_then(|r| r.get(b"XObject").ok())
            .and_then(|x| deref(doc, x).as_dict().ok())
        else {
            return Ok(());
        };
        for name in named {
            // Kept by id, which only a reference has.
            let Ok((Some(id), Object::Stream(stream))) =
                xobjects.get(name).and_then(|o| doc.dereference(o))
            else {
                continue;
            };
            if self.pictures.contains(&id) {
                drawn.pictured = true;
                continue;
            }
            drawn.draws += 1;
            drawn.bytes += self.sizes.get(&id).copied().unwrap_or_default();
            if drawn.draws > DRAWS || drawn.bytes > PAGE_BYTES {
                return Err(too_much());
            }
            if path.contains(&id) {
                return Err("a drawing on it draws itself");
            }
            if path.len() >= DEPTH {
                return Err("its drawings sit too deep within each other");
            }
            let inner = stream
                .dict
                .get(b"Resources")
                .ok()
                .and_then(|r| deref(doc, r).as_dict().ok())
                .or(resources);
            let named = names
                .entry(id)
                .or_insert_with(|| {
                    let content = stream.decompressed_content().unwrap_or_default();
                    let (named, _) = drawings(&content);
                    named
                })
                .clone();
            path.push(id);
            self.draw(&named, inner, path, drawn, names)?;
            path.pop();
        }
        Ok(())
    }
}

fn too_much() -> &'static str {
    "it draws more than a page is read for"
}

/// `object`, or what it refers to.
fn deref<'a>(doc: &'a Document, object: &'a Object) -> &'a Object {
    doc.dereference(object)
        .map_or(&Object::Null, |(_, object)| object)
}

/// The dictionary `key` names in `page` or the nearest of its parents that
/// has one, as the parser looks for it.
fn inherited<'a>(doc: &'a Document, page: &'a Dictionary, key: &[u8]) -> Option<&'a Dictionary> {
    let mut node = page;
    // The loop was ruled out before.
    loop {
        if let Some(found) = node
            .get(key)
            .ok()
            .and_then(|o| deref(doc, o).as_dict().ok())
        {
            return Some(found);
        }
        let parent = node.get(b"Parent").and_then(Object::as_reference).ok()?;
        node = doc.get_dictionary(parent).ok()?;
    }
}

/// The drawings a stream's operations draw by name, and whether it holds a
/// picture of its own.
fn drawings(content: &[u8]) -> (Vec<Vec<u8>>, bool) {
    let Ok(decoded) = pdf_extract::content::Content::decode(content) else {
        return (Vec::new(), false);
    };
    let mut named = Vec::new();
    let mut inline = false;
    for operation in decoded.operations {
        match operation.operator.as_str() {
            "Do" => {
                if let Some(Ok(name)) = operation.operands.first().map(Object::as_name) {
                    named.push(name.to_vec());
                }
            }
            "BI" | "EI" => inline = true,
            _ => {}
        }
    }
    (named, inline)
}

/// The text of page `n`, or why it could not be read.
fn text_of(doc: &Document, n: u32, until: Instant) -> Result<String, String> {
    let mut text = String::new();
    let mut timed = Timed {
        inner: PlainTextOutput::new(&mut text),
        until,
        written: 0,
        late: false,
    };
    let done = crate::diag::contained(|| pdf_extract::output_doc_page(doc, &mut timed, n));
    let late_now = timed.late;
    match done {
        Ok(Ok(())) => Ok(text),
        _ if late_now => Err(format!(
            "reading it took past the {} seconds a read has",
            DEADLINE.as_secs()
        )),
        Ok(Err(e)) => Err(format!("{FAILED} ({e})")),
        Err(_) => Err(FAILED.into()),
    }
}

/// The parser's plain text, stopped once `until` has passed or a page has
/// given more than a view holds.
struct Timed<'a> {
    inner: PlainTextOutput<&'a mut String>,
    until: Instant,
    written: usize,
    late: bool,
}

impl OutputDev for Timed<'_> {
    fn begin_page(
        &mut self,
        page: u32,
        media_box: &MediaBox,
        art_box: Option<(f64, f64, f64, f64)>,
    ) -> Result<(), OutputError> {
        self.inner.begin_page(page, media_box, art_box)
    }
    fn end_page(&mut self) -> Result<(), OutputError> {
        self.inner.end_page()
    }
    fn output_character(
        &mut self,
        trm: &Transform,
        width: f64,
        spacing: f64,
        font_size: f64,
        char: &str,
    ) -> Result<(), OutputError> {
        self.written += char.len();
        // Reading the clock costs more than a character, so every so often.
        if self.written % 1024 < char.len() && Instant::now() >= self.until {
            self.late = true;
            return Err(OutputError::IoError(std::io::Error::other("late")));
        }
        if self.written > super::VIEW_BYTES {
            return Err(OutputError::IoError(std::io::Error::other("full")));
        }
        self.inner
            .output_character(trm, width, spacing, font_size, char)
    }
    fn begin_word(&mut self) -> Result<(), OutputError> {
        self.inner.begin_word()
    }
    fn end_word(&mut self) -> Result<(), OutputError> {
        self.inner.end_word()
    }
    fn end_line(&mut self) -> Result<(), OutputError> {
        self.inner.end_line()
    }
}

/// What a page shows in the view.
enum Shown {
    Text(String),
    /// No text: a scan when it draws a picture, else a page left blank.
    Empty {
        pictured: bool,
    },
    /// Text in a font with no map to letters, which reads as nonsense.
    Garbled,
    Failed(String),
}

/// What `text`, a page's, shows: its lines without the spacing around them,
/// and no more than one empty line in a row.
fn shown(text: String, pictured: bool) -> Shown {
    let mut lines: Vec<&str> = Vec::new();
    for line in text.lines().map(str::trim_end) {
        if !(line.is_empty() && lines.last().is_none_or(|last| last.is_empty())) {
            lines.push(line);
        }
    }
    while lines.last().is_some_and(|last| last.is_empty()) {
        lines.pop();
    }
    if lines.is_empty() {
        return Shown::Empty { pictured };
    }
    let tidy = lines.join("\n");
    // Codes a font without a map gives out: control characters, the
    // private-use ones, and the stand-in for one that could not be read.
    let letters = tidy.chars().filter(|c| !c.is_whitespace()).count();
    let unreadable = tidy
        .chars()
        .filter(|&c| {
            (c.is_control() && c != '\t')
                || ('\u{E000}'..='\u{F8FF}').contains(&c)
                || c >= '\u{F0000}'
                || c == '\u{FFFD}'
        })
        .count();
    if unreadable * 2 > letters {
        return Shown::Garbled;
    }
    Shown::Text(tidy)
}

/// The view of a document of `total` pages, the first of which are `read`.
fn assemble(read: &[Shown], total: usize) -> String {
    let mut view = View::default();
    let pages = |from: usize, to: usize| match from == to {
        true => format!("Page {from}"),
        false => format!("Pages {from}-{to}"),
    };
    let mut at = 0;
    while at < read.len() {
        let n = at + 1;
        // Pages alike with nothing to show go under one heading.
        let alike = |shown: &Shown| match (&read[at], shown) {
            (Shown::Empty { pictured: a }, Shown::Empty { pictured: b }) => a == b,
            (Shown::Garbled, Shown::Garbled) => true,
            _ => false,
        };
        let run = read[at..].iter().take_while(|s| alike(s)).count().max(1);
        let last = at + run;
        let heading = format!("## {} of {total}", pages(n, last));
        if !view.heading(&heading) {
            break;
        }
        let them = pages(n, last);
        let (have, them_or_it) = match run {
            1 => ("has", "it"),
            _ => ("have", "them"),
        };
        let scanned = || {
            let note = SCANNED.replace("them", them_or_it);
            match run {
                1 => note.replace("the pictures", "the picture"),
                _ => note,
            }
        };
        let said = match &read[at] {
            Shown::Text(text) => view.line(text),
            Shown::Empty { pictured: true } => view.line(&format!("[{them} {have} {}]", scanned())),
            Shown::Empty { pictured: false } => view.line(&format!(
                "[{them} {} no text.]",
                if run == 1 { "holds" } else { "hold" }
            )),
            Shown::Garbled => view.line(&format!(
                "[{them} {} letters in a font that gives no way to read them. Render {them_or_it} \
                 to PNG with a tool such as pdftoppm, then read_file the {}.]",
                if run == 1 { "draws its" } else { "draw their" },
                if run == 1 { "picture" } else { "pictures" }
            )),
            Shown::Failed(why) => view.line(&format!("[It could not be read: {why}.]")),
        };
        if !said {
            break;
        }
        at = last;
    }
    if read.len() < total && !view.full {
        view.heading(&format!(
            "[{} not read: reading stopped after {} seconds. `pdftotext` reads the whole file.]",
            match read.len() + 1 == total {
                true => format!("Page {total} is"),
                false => format!("Pages {}-{total} are", read.len() + 1),
            },
            (DEADLINE - MARGIN).as_secs()
        ));
    }
    view.finish()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use pdf_extract::content::{Content, Operation};
    use pdf_extract::{EncryptionState, EncryptionVersion, Permissions};

    /// A dictionary of `pairs`.
    fn dict(pairs: Vec<(&str, Object)>) -> Dictionary {
        let mut dict = Dictionary::new();
        for (key, value) in pairs {
            dict.set(key, value);
        }
        dict
    }

    /// A page's drawing: `lines` one under another in Helvetica, or a
    /// picture alone for a page with none.
    fn drawing(lines: &[&str]) -> Vec<u8> {
        let op = |name: &str, operands: Vec<Object>| Operation::new(name, operands);
        let operations = match lines {
            [] => vec![
                op("q", vec![]),
                op(
                    "cm",
                    vec![
                        200.into(),
                        0.into(),
                        0.into(),
                        200.into(),
                        72.into(),
                        400.into(),
                    ],
                ),
                op("Do", vec!["Im1".into()]),
                op("Q", vec![]),
            ],
            lines => {
                let mut operations = vec![
                    op("BT", vec![]),
                    op("Tf", vec!["F1".into(), 12.into()]),
                    op("TL", vec![14.into()]),
                    op("Td", vec![72.into(), 720.into()]),
                ];
                for line in lines {
                    operations.push(op("Tj", vec![Object::string_literal(*line)]));
                    operations.push(op("T*", vec![]));
                }
                operations.push(op("ET", vec![]));
                operations
            }
        };
        Content { operations }.encode().unwrap()
    }

    /// A document of `pages`, each a page's lines, or a picture alone where
    /// it has none, with `change` made to it before it is saved; given the
    /// pages' ids and those of the resources they share.
    fn document(
        pages: &[&[&str]],
        change: impl FnOnce(&mut Document, &[ObjectId], ObjectId),
    ) -> Vec<u8> {
        let mut doc = Document::with_version("1.5");
        // A plain line, so a PDF of plain text is text a test can serve.
        doc.binary_mark = Vec::new();
        let tree = doc.new_object_id();
        let font = doc.add_object(dict(vec![
            ("Type", "Font".into()),
            ("Subtype", "Type1".into()),
            ("BaseFont", "Helvetica".into()),
        ]));
        let picture = doc.add_object(Stream::new(
            dict(vec![
                ("Type", "XObject".into()),
                ("Subtype", "Image".into()),
                ("Width", 2.into()),
                ("Height", 2.into()),
                ("ColorSpace", "DeviceGray".into()),
                ("BitsPerComponent", 8.into()),
            ]),
            // Grey pixels written as text, so the file is all text.
            b"0110".to_vec(),
        ));
        let resources = doc.add_object(dict(vec![
            ("Font", dict(vec![("F1", font.into())]).into()),
            ("XObject", dict(vec![("Im1", picture.into())]).into()),
        ]));
        let mut kids = Vec::new();
        for lines in pages {
            let contents = doc.add_object(Stream::new(Dictionary::new(), drawing(lines)));
            kids.push(doc.add_object(dict(vec![
                ("Type", "Page".into()),
                ("Parent", tree.into()),
                ("Contents", contents.into()),
                (
                    "MediaBox",
                    vec![0.into(), 0.into(), 612.into(), 792.into()].into(),
                ),
            ])));
        }
        doc.objects.insert(
            tree,
            dict(vec![
                ("Type", "Pages".into()),
                (
                    "Kids",
                    kids.iter()
                        .map(|&kid| kid.into())
                        .collect::<Vec<Object>>()
                        .into(),
                ),
                ("Count", (kids.len() as i64).into()),
                ("Resources", resources.into()),
            ])
            .into(),
        );
        let catalog = doc.add_object(dict(vec![
            ("Type", "Catalog".into()),
            ("Pages", tree.into()),
        ]));
        doc.trailer.set("Root", catalog);
        change(&mut doc, &kids, resources);
        let mut bytes = Vec::new();
        doc.save_to(&mut bytes).unwrap();
        bytes
    }

    /// A document of `pages`, as `document` makes it, unchanged.
    pub(crate) fn pdf(pages: &[&[&str]]) -> Vec<u8> {
        document(pages, |_, _, _| {})
    }

    /// The same, all of it text: where its objects lie is written as a
    /// plain table rather than a packed one.
    pub(crate) fn text_pdf(pages: &[&[&str]]) -> Vec<u8> {
        document(pages, |doc, _, _| {
            doc.reference_table.cross_reference_type =
                pdf_extract::xref::XrefType::CrossReferenceTable;
        })
    }

    /// The view of `bytes` saved as `name` in a folder of its own.
    fn view_of(name: &str, bytes: &[u8]) -> Result<Option<String>, String> {
        let dir = std::env::temp_dir().join(format!("letronna-pdf-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        let view = super::super::view(&path);
        std::fs::remove_dir_all(&dir).unwrap();
        view
    }

    #[test]
    fn a_pdf_reads_page_by_page() {
        let bytes = pdf(&[
            &["Quarterly report", "Sales rose by a third."],
            &["Costs held steady."],
            &["Next steps"],
        ]);
        assert_eq!(
            view_of("report.pdf", &bytes),
            Ok(Some(
                "## Page 1 of 3\nQuarterly report\nSales rose by a third.\n\n\
                 ## Page 2 of 3\nCosts held steady.\n\n\
                 ## Page 3 of 3\nNext steps"
                    .into()
            ))
        );
    }

    #[test]
    fn a_scanned_pdf_says_it_has_no_text_layer() {
        let bytes = pdf(&[&["Cover letter"], &[], &[], &["Signed"], &[]]);
        assert_eq!(
            view_of("scan.pdf", &bytes),
            Ok(Some(
                "## Page 1 of 5\nCover letter\n\n\
                 ## Pages 2-3 of 5\n[Pages 2-3 have no text layer (scanned); read_file cannot \
                 read them. Render them to PNG with a tool such as pdftoppm, then read_file the \
                 pictures.]\n\n\
                 ## Page 4 of 5\nSigned\n\n\
                 ## Page 5 of 5\n[Page 5 has no text layer (scanned); read_file cannot read it. \
                 Render it to PNG with a tool such as pdftoppm, then read_file the picture.]"
                    .into()
            ))
        );
        // A page with neither text nor a picture is blank, not scanned.
        let blank = document(&[&["Only page with words"], &[]], |doc, kids, _| {
            let empty = doc.add_object(Stream::new(Dictionary::new(), Vec::new()));
            doc.get_dictionary_mut(kids[1])
                .unwrap()
                .set("Contents", empty);
        });
        let view = view_of("blank.pdf", &blank).unwrap().unwrap();
        assert!(
            view.ends_with("## Page 2 of 2\n[Page 2 holds no text.]"),
            "{view}"
        );
    }

    /// `bytes` with its text and streams locked under `user`'s password.
    fn locked(bytes: &[u8], user: &str) -> Vec<u8> {
        let mut doc = Document::load_mem(bytes).unwrap();
        doc.trailer.set(
            "ID",
            vec![
                Object::string_literal(b"0123456789abcdef".to_vec()),
                Object::string_literal(b"0123456789abcdef".to_vec()),
            ],
        );
        let state = EncryptionState::try_from(EncryptionVersion::V2 {
            document: &doc,
            owner_password: "owner",
            user_password: user,
            key_length: 128,
            permissions: Permissions::default(),
        })
        .unwrap();
        doc.encrypt(&state).unwrap();
        let mut out = Vec::new();
        doc.save_to(&mut out).unwrap();
        out
    }

    #[test]
    fn an_encrypted_pdf_says_so() {
        let bytes = pdf(&[&["Salaries"]]);
        assert_eq!(
            view_of("pay.pdf", &locked(&bytes, "abc")),
            Err(
                "Could not read this .pdf: it is locked with a password, which read_file cannot \
                 give. With the password, `qpdf --password=<it> --decrypt in.pdf out.pdf` makes \
                 a copy it can read."
                    .into()
            )
        );
        // One locked only against changes opens with no password, as any
        // reader opens it.
        assert_eq!(
            view_of("open.pdf", &locked(&bytes, "")),
            Ok(Some("## Page 1 of 1\nSalaries".into()))
        );
    }

    #[test]
    fn a_truncated_pdf_fails_within_its_deadline() {
        let bytes = pdf(&[&["Whole"], &["Document"]]);
        let started = Instant::now();
        for cut in [bytes.len() * 3 / 10, bytes.len() * 6 / 10, bytes.len() - 40] {
            let error = view_of("cut.pdf", &bytes[..cut]).unwrap_err();
            assert!(
                error.starts_with("Could not read this .pdf: it is damaged or cut short ("),
                "{error}"
            );
            assert!(
                error.ends_with("; `pdftotext` may still read it."),
                "{error}"
            );
        }
        assert!(started.elapsed() < DEADLINE, "{:?}", started.elapsed());
    }

    #[test]
    fn a_pdf_over_its_limit_is_refused_unread() {
        let dir = std::env::temp_dir().join(format!("letronna-pdf-big-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("scan.pdf");
        std::fs::write(&path, b"%PDF-1.7\n").unwrap();
        // Sparse, so the test writes nothing near that size.
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_len(FILE_BYTES + 1)
            .unwrap();
        assert_eq!(
            super::super::view(&path),
            Err(
                "Could not read this .pdf: it is over 50 MB, more than read_file reads; \
                 `pdftotext` reads one that size."
                    .into()
            )
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn paging_a_pdf_does_not_parse_it_again() {
        let dir = std::env::temp_dir().join(format!("letronna-pdf-kept-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("kept.pdf");
        std::fs::write(&path, pdf(&[&["Kept AAAA"]])).unwrap();
        let first = super::super::view(&path).unwrap().unwrap();
        assert_eq!(first, "## Page 1 of 1\nKept AAAA");
        // The same length and time, with other words: still the view read
        // first, so the file was not read again.
        let when = std::fs::metadata(&path).unwrap().modified().unwrap();
        std::fs::write(&path, pdf(&[&["Kept BBBB"]])).unwrap();
        File::options()
            .write(true)
            .open(&path)
            .unwrap()
            .set_modified(when)
            .unwrap();
        assert_eq!(super::super::view(&path).unwrap().unwrap(), first);
        // Changed as files change, it is read again.
        std::fs::write(&path, pdf(&[&["Changed"]])).unwrap();
        assert_eq!(
            super::super::view(&path).unwrap().unwrap(),
            "## Page 1 of 1\nChanged"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_page_the_parser_panics_on_costs_only_that_page() {
        let bytes = document(&[&["Before"], &["Lost"], &["After"]], |doc, kids, _| {
            let page = doc.get_dictionary_mut(kids[1]).unwrap();
            page.set(
                "MediaBox",
                vec![
                    0.into(),
                    0.into(),
                    Object::string_literal("wide"),
                    792.into(),
                ],
            );
        });
        assert_eq!(
            view_of("panics.pdf", &bytes),
            Ok(Some(
                "## Page 1 of 3\nBefore\n\n\
                 ## Page 2 of 3\n[It could not be read: its reader failed on it.]\n\n\
                 ## Page 3 of 3\nAfter"
                    .into()
            ))
        );
    }

    #[test]
    fn a_drawing_that_draws_itself_or_too_much_is_not_followed() {
        // Followed by the parser, it would draw itself until the stack ran
        // out and ended the process.
        let bytes = document(&[&[], &["Fine"], &[]], |doc, kids, resources| {
            let form = doc.new_object_id();
            let inner = dict(vec![("XObject", dict(vec![("Fm1", form.into())]).into())]);
            let draws = |times: usize| {
                let mut content = Vec::new();
                for _ in 0..times {
                    content.extend_from_slice(b"/Fm1 Do\n");
                }
                content
            };
            doc.objects.insert(
                form,
                Stream::new(
                    dict(vec![
                        ("Type", "XObject".into()),
                        ("Subtype", "Form".into()),
                        (
                            "BBox",
                            vec![0.into(), 0.into(), 10.into(), 10.into()].into(),
                        ),
                        ("Resources", inner.into()),
                    ]),
                    draws(1),
                )
                .into(),
            );
            let shared = doc.get_dictionary_mut(resources).unwrap();
            let xobjects = shared.get_mut(b"XObject").unwrap().as_dict_mut().unwrap();
            xobjects.set("Fm1", form);
            let first = doc.add_object(Stream::new(Dictionary::new(), draws(1)));
            doc.get_dictionary_mut(kids[0])
                .unwrap()
                .set("Contents", first);
            // A form that draws nothing, drawn more times than a page is read for.
            let empty = doc.add_object(Stream::new(
                dict(vec![
                    ("Type", "XObject".into()),
                    ("Subtype", "Form".into()),
                    (
                        "BBox",
                        vec![0.into(), 0.into(), 10.into(), 10.into()].into(),
                    ),
                ]),
                Vec::new(),
            ));
            let many = doc.add_object(dict(vec![(
                "XObject",
                dict(vec![("Fm1", empty.into())]).into(),
            )]));
            let last = doc.add_object(Stream::new(Dictionary::new(), draws(DRAWS + 1)));
            let page = doc.get_dictionary_mut(kids[2]).unwrap();
            page.set("Contents", last);
            page.set("Resources", many);
        });
        assert_eq!(
            view_of("loops.pdf", &bytes),
            Ok(Some(
                "## Page 1 of 3\n[It could not be read: a drawing on it draws itself.]\n\n\
                 ## Page 2 of 3\nFine\n\n\
                 ## Page 3 of 3\n[It could not be read: it draws more than a page is read for.]"
                    .into()
            ))
        );
    }

    #[test]
    fn a_page_whose_parents_loop_is_not_followed() {
        // It inherits its size, so the parser would look for it up the loop
        // forever.
        let bytes = document(&[&["Looped"], &["Fine"]], |doc, kids, _| {
            let between = doc.add_object(dict(vec![
                ("Type", "Pages".into()),
                ("Parent", kids[0].into()),
            ]));
            let page = doc.get_dictionary_mut(kids[0]).unwrap();
            page.remove(b"MediaBox");
            page.set("Parent", between);
        });
        assert_eq!(
            view_of("parents.pdf", &bytes),
            Ok(Some(
                "## Page 1 of 2\n[It could not be read: its place in the list of pages loops.]\n\n\
                 ## Page 2 of 2\nFine"
                    .into()
            ))
        );
    }

    /// `data` packed as a PDF's FlateDecode packs it.
    fn deflated(data: &[u8]) -> Vec<u8> {
        use std::io::Write;
        let mut packer = flate2::write::ZlibEncoder::new(Vec::new(), flate2::Compression::fast());
        packer.write_all(data).unwrap();
        packer.finish().unwrap()
    }

    #[test]
    fn a_stream_unpacking_past_its_limit_is_refused_before_it_is_unpacked() {
        let zeros = vec![0; STREAM_BYTES as usize + 1];
        let packed = deflated(&zeros);
        let over = "Could not read this .pdf: a part of it unpacks to more than 64 MB, more than \
                    any page's text needs.";
        // A page's drawing.
        let page = document(&[&["Small"]], |doc, kids, _| {
            let mut stream = Stream::new(dict(vec![("Filter", "FlateDecode".into())]), Vec::new());
            stream.set_content(packed.clone());
            let bomb = doc.add_object(stream);
            doc.get_dictionary_mut(kids[0])
                .unwrap()
                .set("Contents", bomb);
        });
        assert_eq!(view_of("bomb.pdf", &page), Err(over.into()));
        // A table of where the objects lie, which the parser unpacks first.
        let mut table = b"%PDF-1.5\n1 0 obj\n<< /Type /XRef /Size 2 /W [1 2 1] /Filter /FlateDecode >>\nstream\n".to_vec();
        table.extend_from_slice(&packed);
        table.extend_from_slice(b"\nendstream\nendobj\nstartxref\n9\n%%EOF\n");
        assert_eq!(view_of("table.pdf", &table), Err(over.into()));
        assert_eq!(
            unpacked(&Stream::new(Dictionary::new(), vec![1; 9]), 8),
            None
        );
        let mut lzw = Stream::new(dict(vec![("Filter", "LZWDecode".into())]), Vec::new());
        lzw.set_content(
            weezl::encode::Encoder::with_tiff_size_switch(weezl::BitOrder::Msb, 8)
                .encode(&zeros[..1024 * 1024])
                .unwrap(),
        );
        assert_eq!(unpacked(&lzw, 1024 * 1024), Some(1024 * 1024));
        assert_eq!(unpacked(&lzw, 1024 * 1024 - 1), None);
    }

    #[test]
    fn text_in_a_font_with_no_map_to_letters_is_named_as_such() {
        assert!(matches!(
            shown("\u{1}\u{2}\u{3}\u{E001} ab".into(), false),
            Shown::Garbled
        ));
        assert!(
            matches!(shown("Price: 5 €\n\n\n".into(), false), Shown::Text(text) if text == "Price: 5 €")
        );
        assert!(matches!(
            shown(" \n \n".into(), true),
            Shown::Empty { pictured: true }
        ));
    }
}
