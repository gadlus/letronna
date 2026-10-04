//! Word, Excel, PowerPoint and PDF files, and OpenDocument's text,
//! spreadsheets and presentations, as read_file shows them: a document's
//! text, a workbook's sheets as rows of the values they last showed, a
//! deck's slides with their notes, and a PDF's pages, in place of the zip of
//! XML or the PDF on disk.
//!
//! A Word heading becomes `#` marks and a table markdown rows. Each sheet
//! opens with `## Sheet "Name" (R rows x C cols)`, its rows following one
//! line each from row 1, cells apart by tabs from column A, so a line and a
//! field name a cell; only a row longer than read_file shows on one line
//! goes on to the next. Each slide opens with `## Slide N`, its notes under
//! `### Notes`. Styling, formulas, pictures and charts are left out, so the
//! view is for reading only, and nothing writes one back.
//!
//! Documents mostly come from someone else, and one can be made to hurt its
//! reader, so each is read within limits -- the files it holds, what its
//! parts unpack to, the rows and columns of a sheet, the size of the view --
//! on a thread of its own, which a deadline stops waiting for. The last few
//! views are kept, so paging through a document reads it once.

mod odf;
mod pdf;

use quick_xml::XmlVersion;
use quick_xml::events::{BytesRef, BytesStart, Event};
use quick_xml::reader::Reader;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Mutex;
use std::sync::mpsc::RecvTimeoutError;
use std::time::{Duration, Instant, SystemTime};
use zip::ZipArchive;
use zip::result::ZipError;

/// The most a document's parts may unpack to together, in bytes. The XML
/// of a long report or a deck of hundreds of slides is a few MB; a zip bomb
/// says it holds gigabytes, and is refused on its word before anything is
/// inflated.
const UNPACKED_BYTES: u64 = 50 * 1024 * 1024;
/// The most files a document may hold. A deck of a thousand slides, each
/// with notes and pictures, holds about five thousand; a zip with more is
/// made to cost its reader, not to be read.
const ENTRIES: usize = 10_000;
/// The most read of a document to find the files in its zip and list them:
/// `ENTRIES` files take about 1.5 MB, each listed in under a hundred bytes
/// and its name, where a list made to cost its reader runs to hundreds and
/// would be read whole before `ENTRIES` could refuse it.
const LISTING_BYTES: u64 = 4 * 1024 * 1024;
/// Rows of a sheet shown: a table's worth to read or search through, where
/// a longer one is data for a program to load rather than to page through.
const ROWS: u32 = 5_000;
/// Columns of a sheet shown: A to IV, all Excel had until 2007, past which
/// a sheet is mostly empty.
const COLUMNS: u32 = 256;
/// The most the view may hold, in bytes: thousands of read_file pages, so
/// only a cell repeated through a sheet, or tables nested deep in each
/// other, which could otherwise make it gigabytes, ever meet it.
const VIEW_BYTES: usize = 16 * 1024 * 1024;
/// How long a read may take: far longer than the limits above let one run.
/// Past it the call gives up, and the reader stops at its next step through
/// the XML rather than run on for a way past them nobody saw.
const DEADLINE: Duration = Duration::from_secs(20);
/// The stack a reader runs on: a PDF's parser goes down drawings within
/// drawings by calling itself, so room for those a page may hold.
const STACK_BYTES: usize = 64 * 1024 * 1024;
/// How many views are kept, each with the file as it was when read.
const KEPT_VIEWS: usize = 4;
/// Elements whose text a reader does not see: deleted and moved-away text,
/// the codes behind fields, and the copy of a drawing kept for programs
/// that cannot show it, which would say a text box twice.
const LEFT_OUT: [&[u8]; 5] = [b"del", b"moveFrom", b"delText", b"instrText", b"Fallback"];

/// What a document is, by its extension.
#[derive(Clone, Copy)]
enum Kind {
    Word,
    Excel,
    PowerPoint,
    Pdf,
    /// OpenDocument's, each read into the view of its Office kin.
    Text,
    Spreadsheet,
    Presentation,
}

/// The kind of document `path` is named as.
fn kind(path: &Path) -> Option<Kind> {
    kind_of(&path.extension()?.to_string_lossy())
}

/// The kind of document a file ending in `extension`, such as "pdf", is.
fn kind_of(extension: &str) -> Option<Kind> {
    // Each with its macro-enabled kind and its templates, and a deck with
    // the shows that open playing: the same zips, the same parts.
    match extension.to_ascii_lowercase().as_str() {
        "docx" | "docm" | "dotx" | "dotm" => Some(Kind::Word),
        "xlsx" | "xlsm" | "xltx" | "xltm" => Some(Kind::Excel),
        "pptx" | "pptm" | "potx" | "potm" | "ppsx" | "ppsm" => Some(Kind::PowerPoint),
        "pdf" => Some(Kind::Pdf),
        // OpenDocument's, each with its template.
        "odt" | "ott" => Some(Kind::Text),
        "ods" | "ots" => Some(Kind::Spreadsheet),
        "odp" | "otp" => Some(Kind::Presentation),
        _ => None,
    }
}

/// Whether `path` is named as a document read_file shows as a view.
pub(crate) fn named(path: &Path) -> bool {
    kind(path).is_some()
}

/// Whether a file ending in `extension`, such as "pdf", is such a document.
pub(crate) fn is_extension(extension: &str) -> bool {
    kind_of(extension).is_some()
}

/// The lines of `lines`, the view of the document at `path`, that say where
/// in it the lines after them are: a PDF's pages, a workbook's sheets, a
/// deck's slides, or a Word document's headings.
pub(crate) fn places(path: &Path, lines: &[&[u8]]) -> Vec<usize> {
    let starts: &[&[u8]] = match kind(path) {
        Some(Kind::Pdf) => &[b"## Page "],
        Some(Kind::Excel | Kind::Spreadsheet) => &[b"## Sheet \""],
        Some(Kind::PowerPoint | Kind::Presentation) => &[b"## Slide "],
        Some(Kind::Word | Kind::Text) => &[b"# ", b"## ", b"### ", b"#### ", b"##### ", b"###### "],
        None => &[],
    };
    (0..lines.len())
        .filter(|&n| starts.iter().any(|start| lines[n].starts_with(start)))
        .collect()
}

/// The plain kind, such as "docx", that a file named as an Office document
/// converts to; None for one not named as one.
pub(crate) fn plain(path: &Path) -> Option<&'static str> {
    Some(match kind(path)? {
        Kind::Word => "docx",
        Kind::Excel => "xlsx",
        Kind::PowerPoint => "pptx",
        // OpenDocument's lock with a password inside the zip, so one that
        // is no zip was never one.
        Kind::Pdf | Kind::Text | Kind::Spreadsheet | Kind::Presentation => return None,
    })
}

/// The view of the document at `path`; None for a file not named as one,
/// or not a zip or a PDF as one is, which is read as any other file.
pub(crate) fn view(path: &Path) -> Result<Option<String>, String> {
    view_by(path, Instant::now() + DEADLINE)
}

/// The view as `view` gives it, given up on at `until` when that comes
/// before `DEADLINE` would: a search has a budget of its own.
pub(crate) fn view_by(path: &Path, until: Instant) -> Result<Option<String>, String> {
    let Some(kind) = kind(path) else {
        return Ok(None);
    };
    let mut start = [0; 1024];
    let read_in = File::open(path).and_then(|mut file| file.read(&mut start));
    let start = &start[..read_in.unwrap_or_default()];
    let opens = match kind {
        // A PDF may have a few bytes before its header.
        Kind::Pdf => start.windows(5).any(|w| w == b"%PDF-"),
        _ => start.starts_with(b"PK\x03\x04"),
    };
    if !opens {
        return Ok(None);
    }
    let stamp = std::fs::metadata(path)
        .ok()
        .map(|meta| (meta.len(), meta.modified().ok()));
    if let Some(kept) = stamp.and_then(|stamp| kept(path, stamp)) {
        return Ok(Some(kept));
    }
    let owned = path.to_path_buf();
    let until = until.min(Instant::now() + DEADLINE);
    let (send, wait) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("read-document".into())
        .stack_size(STACK_BYTES)
        .spawn(move || {
            // A parser's panic is the file's doing, so it is no crash.
            let read = crate::diag::contained(|| read(&owned, kind, until))
                .unwrap_or_else(|_| Err(FAILED.into()));
            let _ = send.send(read);
        })
        .map_err(|e| e.to_string())?;
    // Past the deadline the reader stops itself at its next step: through
    // the XML, or a PDF's next page or character.
    let outcome = match wait.recv_timeout(until.saturating_duration_since(Instant::now())) {
        Ok(outcome) => outcome,
        Err(RecvTimeoutError::Timeout) => Err(late()),
        Err(RecvTimeoutError::Disconnected) => Err(FAILED.into()),
    };
    let extension = path.extension().unwrap_or_default().to_string_lossy();
    let view = outcome.map_err(|why| {
        format!(
            "Could not read this .{}: {why}.",
            extension.to_ascii_lowercase()
        )
    })?;
    if let Some(stamp) = stamp {
        keep(path, stamp, &view);
    }
    Ok(Some(view))
}

/// A file as it was when its view was read: its length and when it changed.
type Stamp = (u64, Option<SystemTime>);

/// The views read last, the newest first.
static KEPT: Mutex<Vec<(PathBuf, Stamp, String)>> = Mutex::new(Vec::new());

/// The view kept for `path` while it is as `stamp` says.
fn kept(path: &Path, stamp: Stamp) -> Option<String> {
    let kept = KEPT.lock().ok()?;
    kept.iter()
        .find(|(at, was, _)| at == path && *was == stamp)
        .map(|(_, _, view)| view.clone())
}

/// Keeps `view` of `path`, as `stamp` says it is, among the last few.
fn keep(path: &Path, stamp: Stamp, view: &str) {
    let Ok(mut kept) = KEPT.lock() else {
        return;
    };
    kept.retain(|(at, _, _)| at != path);
    kept.insert(0, (path.to_path_buf(), stamp, view.to_owned()));
    kept.truncate(KEPT_VIEWS);
}

/// Why a read whose parser panicked failed.
const FAILED: &str = "its reader failed on it";

/// Why a read that ran past `DEADLINE` failed.
fn late() -> String {
    format!("it took over {} seconds", DEADLINE.as_secs())
}

/// Whether `error`, from `view_by`, says the read ran out of time rather
/// than that the file could not be read.
pub(crate) fn ran_late(error: &str) -> bool {
    error.ends_with(&format!(": {}.", late()))
}

/// The view of the document at `path`, read as a `kind` until `until`.
fn read(path: &Path, kind: Kind, until: Instant) -> Result<String, String> {
    let read_parts = match kind {
        Kind::Word => document,
        Kind::Excel => workbook,
        Kind::PowerPoint => deck,
        Kind::Text => odf::text,
        Kind::Spreadsheet => odf::spreadsheet,
        Kind::Presentation => odf::presentation,
        Kind::Pdf => return pdf::read(path, until),
    };
    let mut package = Package::open(path, until)?;
    let mut view = View::default();
    read_parts(&mut package, &mut view)?;
    Ok(view.finish())
}

/// A document's file as its zip reads it, which gives out no more than
/// `LISTING_BYTES` while the files in it are found and listed.
struct Source {
    file: BufReader<File>,
    /// What may still be read; `u64::MAX` once the files are listed.
    left: Rc<std::cell::Cell<u64>>,
}

impl Read for Source {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        let left = self.left.get();
        // An error, not the end of the file, which would only send the zip
        // looking for its list somewhere else.
        if left == 0 {
            return Err(std::io::Error::other("the listing limit was reached"));
        }
        let most = buf.len().min(usize::try_from(left).unwrap_or(usize::MAX));
        let n = self.file.read(&mut buf[..most])?;
        self.left.set(left - n as u64);
        Ok(n)
    }
}

impl Seek for Source {
    fn seek(&mut self, to: SeekFrom) -> std::io::Result<u64> {
        self.file.seek(to)
    }
}

/// A document's zip, how much more its parts may unpack to, and when its
/// reading has to stop.
struct Package {
    zip: ZipArchive<Source>,
    left: u64,
    until: Instant,
}

impl Package {
    /// Opens the zip at `path`, to be read until `until`; refused when
    /// listing its files takes more than `LISTING_BYTES`, or it holds more
    /// than `ENTRIES` of them.
    fn open(path: &Path, until: Instant) -> Result<Package, String> {
        let file = File::open(path).map_err(|e| e.to_string())?;
        let listing = Rc::new(std::cell::Cell::new(LISTING_BYTES));
        let source = Source {
            file: BufReader::new(file),
            left: Rc::clone(&listing),
        };
        let zip = ZipArchive::new(source).map_err(|e| match listing.get() {
            0 => format!(
                "the list of the files in it runs past {} MB, longer than any document's",
                LISTING_BYTES / (1024 * 1024)
            ),
            _ => format!("it is damaged or cut short ({e})"),
        })?;
        // Its parts are read within limits of their own.
        listing.set(u64::MAX);
        if zip.len() > ENTRIES {
            return Err(format!(
                "it holds {} files, more than any document",
                zip.len()
            ));
        }
        Ok(Package {
            zip,
            left: UNPACKED_BYTES,
            until,
        })
    }

    /// The part `name`, unpacked; None when the document has none. A part
    /// that says it unpacks to more than is left is refused before any of it
    /// is inflated.
    fn part(&mut self, name: &str) -> Result<Option<Vec<u8>>, String> {
        let mut file = match self.zip.by_name(name) {
            Ok(file) => file,
            Err(ZipError::FileNotFound) => return Ok(None),
            Err(e) => return Err(format!("its {name} cannot be opened ({e})")),
        };
        let size = file.size();
        if size > self.left {
            return Err(format!(
                "it unpacks to more than {} MB",
                UNPACKED_BYTES / (1024 * 1024)
            ));
        }
        let mut bytes = Vec::new();
        // One byte past what it says tells a part that unpacks to more.
        file.by_ref()
            .take(size + 1)
            .read_to_end(&mut bytes)
            .map_err(|e| format!("its {name} is damaged ({e})"))?;
        if bytes.len() as u64 > size {
            return Err(format!("its {name} unpacks to more than it says"));
        }
        self.left -= size;
        Ok(Some(bytes))
    }

    /// Walks the part `name` as it unpacks, up to what is left to unpack: a
    /// sheet, of which only the first rows are shown however large it is.
    /// True when it ran out there.
    fn stream(&mut self, name: &str, each: impl FnMut(Node) -> bool) -> Result<bool, String> {
        let file = self
            .zip
            .by_name(name)
            .map_err(|e| format!("its {name} cannot be opened ({e})"))?;
        let mut source = BufReader::new(file.take(self.left));
        let walked = walk(&mut source, self.until, each);
        self.left = source.get_ref().limit();
        match walked {
            // Cut where nothing is left, the XML can end in the middle of a tag.
            _ if self.left == 0 => Ok(true),
            Ok(()) => Ok(false),
            Err(why) => Err(why),
        }
    }
}

/// One step through a part's XML.
enum Node<'a> {
    /// An element opens; an empty one closes straight after.
    Open(&'a BytesStart<'a>),
    /// An element closes, named without its prefix.
    Close(&'a [u8]),
    /// Text, its references resolved.
    Text(&'a str),
}

/// Walks the XML from `source`, handing `each` every step until it says to
/// stop; failed once `until` has passed.
fn walk(
    source: impl BufRead,
    until: Instant,
    mut each: impl FnMut(Node) -> bool,
) -> Result<(), String> {
    let broken = |e: &dyn std::fmt::Display| format!("its XML is broken ({e})");
    let mut reader = Reader::from_reader(source);
    let mut buf = Vec::new();
    loop {
        // Every step does a bounded amount of work, and reading the clock
        // costs less than one, so the reader stops soon after the deadline.
        if Instant::now() >= until {
            return Err(late());
        }
        let event = reader.read_event_into(&mut buf).map_err(|e| broken(&e))?;
        let go = match &event {
            Event::Start(tag) => each(Node::Open(tag)),
            Event::Empty(tag) => {
                each(Node::Open(tag)) && each(Node::Close(tag.local_name().into_inner()))
            }
            Event::End(tag) => each(Node::Close(tag.local_name().into_inner())),
            Event::Text(text) => each(Node::Text(&text.xml10_content().map_err(|e| broken(&e))?)),
            Event::CData(text) => each(Node::Text(&text.decode().map_err(|e| broken(&e))?)),
            Event::GeneralRef(name) => match reference(name) {
                Some(text) => each(Node::Text(&text)),
                None => true,
            },
            Event::Eof => return Ok(()),
            _ => true,
        };
        if !go {
            return Ok(());
        }
        buf.clear();
    }
}

/// What a reference such as `&amp;` or `&#169;` stands for.
fn reference(name: &BytesRef) -> Option<String> {
    if let Ok(Some(c)) = name.resolve_char_ref() {
        return Some(c.to_string());
    }
    let name = name.decode().ok()?;
    quick_xml::escape::resolve_xml_entity(&name).map(str::to_owned)
}

/// The name of `tag` without its prefix.
fn local<'a>(tag: &'a BytesStart) -> &'a [u8] {
    tag.local_name().into_inner()
}

/// The value of `tag`'s attribute `name`, whatever its prefix.
fn attr(tag: &BytesStart, name: &[u8]) -> Option<String> {
    let found = tag
        .attributes()
        .flatten()
        .find(|attribute| attribute.key.local_name().as_ref() == name)?;
    let value = found.normalized_value(XmlVersion::Implicit1_0).ok()?;
    Some(value.into_owned())
}

/// The relationship `tag` points to: its `r:id`, told from a plain `id` by
/// its prefix.
fn link_id(tag: &BytesStart) -> Option<String> {
    let found = tag.attributes().flatten().find(|attribute| {
        attribute.key.prefix().is_some() && attribute.key.local_name().as_ref() == b"id"
    })?;
    let value = found.normalized_value(XmlVersion::Implicit1_0).ok()?;
    Some(value.into_owned())
}

/// A relationship from one part to another: its id, its type, and the part
/// it names.
struct Link {
    id: String,
    kind: String,
    target: String,
}

/// The relationships of the part named `of`, from the `_rels` file beside
/// it; the package's own for "".
fn links_of(package: &mut Package, of: &str) -> Result<Vec<Link>, String> {
    let (folder, name) = of.rsplit_once('/').unwrap_or(("", of));
    let rels = match folder {
        "" => format!("_rels/{name}.rels"),
        _ => format!("{folder}/_rels/{name}.rels"),
    };
    let Some(bytes) = package.part(&rels)? else {
        return Ok(Vec::new());
    };
    let mut links = Vec::new();
    walk(&bytes[..], package.until, |node| {
        if let Node::Open(tag) = node
            && local(tag) == b"Relationship"
            && attr(tag, b"TargetMode").is_none_or(|mode| mode != "External")
            && let (Some(id), Some(kind), Some(target)) =
                (attr(tag, b"Id"), attr(tag, b"Type"), attr(tag, b"Target"))
        {
            let target = resolve(folder, &target);
            links.push(Link { id, kind, target });
        }
        true
    })?;
    Ok(links)
}

/// The part the first of `links` of type `kind`, such as "styles", names.
fn linked<'a>(links: &'a [Link], kind: &str) -> Option<&'a str> {
    links
        .iter()
        .find(|link| link.kind.rsplit('/').next() == Some(kind))
        .map(|link| link.target.as_str())
}

/// `links` by id, the first of any that share one. Found once, as a part
/// can list a slide or sheet for each of its links, and matching each
/// against every link would take minutes after the call gave up.
fn by_id(links: &[Link]) -> HashMap<&str, &Link> {
    let mut found = HashMap::with_capacity(links.len());
    for link in links {
        found.entry(link.id.as_str()).or_insert(link);
    }
    found
}

/// `target` as a name in the zip: from the top when it starts with `/`,
/// else from `folder`, with each `..` going up one.
fn resolve(folder: &str, target: &str) -> String {
    let mut parts: Vec<&str> = if target.starts_with('/') {
        Vec::new()
    } else {
        folder.split('/').filter(|part| !part.is_empty()).collect()
    };
    for part in target.split('/') {
        match part {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            part => parts.push(part),
        }
    }
    parts.join("/")
}

/// The part the package names as its main one, or `usual`, where Office
/// puts it.
fn main_part(package: &mut Package, usual: &str) -> Result<String, String> {
    let links = links_of(package, "")?;
    Ok(linked(&links, "officeDocument").unwrap_or(usual).to_owned())
}

/// The view as it is built, which takes no more once it reaches
/// `VIEW_BYTES`.
#[derive(Default)]
struct View {
    text: String,
    started: bool,
    full: bool,
}

impl View {
    /// Adds `text` as lines, each longer than read_file shows split at a
    /// space where there is one, so no line of the view is cut; false once
    /// the view is full.
    fn line(&mut self, text: &str) -> bool {
        for line in text.split('\n') {
            let mut rest = line.trim_end_matches('\r');
            loop {
                if self.full || self.text.len() + rest.len() >= VIEW_BYTES {
                    self.full = true;
                    return false;
                }
                let Some((cut, _)) = rest.char_indices().nth(crate::tools::LINE_CHARS) else {
                    self.push(rest);
                    break;
                };
                // After the last space in the second half, so a word stays whole.
                let at = rest[..cut]
                    .rfind(' ')
                    .filter(|&at| at > cut / 2)
                    .map_or(cut, |at| at + 1);
                self.push(rest[..at].trim_end());
                rest = &rest[at..];
            }
        }
        true
    }

    /// Adds `heading` with an empty line above it, unless it comes first.
    fn heading(&mut self, heading: &str) -> bool {
        (!self.started || self.line("")) && self.line(heading)
    }

    fn push(&mut self, line: &str) {
        if self.started {
            self.text.push('\n');
        }
        self.text.push_str(line);
        self.started = true;
    }

    /// The view as read_file pages it.
    fn finish(mut self) -> String {
        if self.full {
            let mb = VIEW_BYTES / (1024 * 1024);
            if self.started {
                self.text.push_str("\n\n");
            }
            self.text.push_str(&format!(
                "[The rest is not shown: the view stops at {mb} MB.]"
            ));
        }
        if self.text.trim().is_empty() {
            return "[This document holds no text.]".into();
        }
        self.text
    }
}

/// A Word document's text: its paragraphs, headings, lists and tables.
fn document(package: &mut Package, view: &mut View) -> Result<(), String> {
    let main = main_part(package, "word/document.xml")?;
    let links = links_of(package, &main)?;
    let mut headings = HashMap::new();
    if let Some(styles) = linked(&links, "styles")
        && let Some(bytes) = package.part(styles)?
    {
        headings = heading_styles(&bytes, package.until)?;
    }
    let Some(bytes) = package.part(&main)? else {
        return Err(format!("it has no {main}"));
    };
    let mut prose = Prose {
        headings,
        ..Prose::default()
    };
    walk(&bytes[..], package.until, |node| prose.take(node))?;
    for line in &prose.lines {
        if !view.line(line) {
            break;
        }
    }
    view.full |= prose.full;
    Ok(())
}

/// The heading level of each paragraph style in Word's styles part, by the
/// id paragraphs name it by: a style keeps its English name there whatever
/// language its id is in.
fn heading_styles(bytes: &[u8], until: Instant) -> Result<HashMap<String, usize>, String> {
    let mut levels = HashMap::new();
    let mut style = None;
    walk(bytes, until, |node| {
        let Node::Open(tag) = node else {
            return true;
        };
        match local(tag) {
            b"style" => style = attr(tag, b"styleId"),
            b"name" => {
                let level = attr(tag, b"val").and_then(|name| heading_level(&name));
                if let (Some(id), Some(level)) = (&style, level) {
                    levels.insert(id.clone(), level);
                }
            }
            _ => {}
        }
        true
    })?;
    Ok(levels)
}

/// The heading level a paragraph style gives by its name or id, such as
/// "heading 2" or "Heading2"; a title counts as the first level.
fn heading_level(style: &str) -> Option<usize> {
    let style = style.to_ascii_lowercase();
    if style == "title" {
        return Some(1);
    }
    let level: usize = style.strip_prefix("heading")?.trim().parse().ok()?;
    // Markdown goes six levels deep, where Word goes nine.
    (level > 0).then_some(level.min(6))
}

/// Paragraphs and tables of Word or slide XML as lines, in the order they
/// end.
#[derive(Default)]
struct Prose {
    /// The heading level of each of the document's paragraph styles, by id.
    headings: HashMap<String, usize>,
    /// Whether only the text in a notes page's body counts, without its
    /// picture of the slide, its number or its date.
    notes: bool,
    lines: Vec<String>,
    /// Whether the slide is hidden in a show.
    hidden: bool,
    /// How deep the walk is in an element of `LEFT_OUT`.
    skip: usize,
    paragraphs: Vec<Paragraph>,
    tables: Vec<Table>,
    /// For each shape open, whether it is the body of a notes page.
    shapes: Vec<bool>,
    /// Whether the walk is in a `t`, where the text is.
    text: bool,
    /// Bytes of the lines finished, a table's counted again each time they
    /// go into a cell of the table around it.
    bytes: usize,
    /// Whether those passed `VIEW_BYTES`, which stops the walk.
    full: bool,
}

/// A paragraph being read.
#[derive(Default)]
struct Paragraph {
    text: String,
    /// Its heading level, 0 for none.
    heading: usize,
    /// How deep in a list it is, when it is a list item.
    list: Option<usize>,
    run: Option<Run>,
}

/// A run of text in one format, which a hidden format leaves out.
#[derive(Default)]
struct Run {
    text: String,
    hidden: bool,
}

/// A table being read: its rows of cells, and the cell open.
#[derive(Default)]
struct Table {
    rows: Vec<Vec<String>>,
    cell: Option<String>,
}

impl Prose {
    /// Takes the next step of the walk; true until the lines are full.
    fn take(&mut self, node: Node) -> bool {
        if self.skip > 0 {
            match node {
                Node::Open(_) => self.skip += 1,
                Node::Close(_) => self.skip -= 1,
                Node::Text(_) => {}
            }
            return true;
        }
        match node {
            Node::Open(tag) => self.open(tag),
            Node::Close(name) => self.close(name),
            Node::Text(text) if self.text => self.push(text),
            Node::Text(_) => {}
        }
        !self.full
    }

    fn open(&mut self, tag: &BytesStart) {
        let name = local(tag);
        if LEFT_OUT.contains(&name) {
            self.skip = 1;
            return;
        }
        match name {
            b"p" => self.paragraphs.push(Paragraph::default()),
            b"r" => {
                if let Some(paragraph) = self.paragraphs.last_mut() {
                    paragraph.run = Some(Run::default());
                }
            }
            b"t" => self.text = true,
            // Outside a run a tab is a tab stop, not a character.
            b"tab" => {
                if let Some(run) = self.paragraphs.last_mut().and_then(|p| p.run.as_mut()) {
                    run.text.push('\t');
                }
            }
            b"br" | b"cr" => {
                if !matches!(attr(tag, b"type").as_deref(), Some("page" | "column")) {
                    self.push("\n");
                }
            }
            b"noBreakHyphen" => self.push("-"),
            b"vanish" => {
                if let Some(run) = self.paragraphs.last_mut().and_then(|p| p.run.as_mut()) {
                    let shown = matches!(attr(tag, b"val").as_deref(), Some("0" | "false" | "off"));
                    run.hidden = !shown;
                }
            }
            b"pStyle" => {
                let level = attr(tag, b"val").and_then(|id| {
                    self.headings
                        .get(&id)
                        .copied()
                        .or_else(|| heading_level(&id))
                });
                if let Some(paragraph) = self.paragraphs.last_mut() {
                    paragraph.heading = level.unwrap_or(0);
                }
            }
            b"numPr" | b"ilvl" => {
                let depth = attr(tag, b"val").and_then(|depth| depth.parse().ok());
                if let Some(paragraph) = self.paragraphs.last_mut() {
                    paragraph.list = Some(depth.unwrap_or(0));
                }
            }
            b"tbl" => self.tables.push(Table::default()),
            b"tr" => {
                if let Some(table) = self.tables.last_mut() {
                    table.rows.push(Vec::new());
                }
            }
            b"tc" => {
                if let Some(table) = self.tables.last_mut() {
                    table.cell = Some(String::new());
                }
            }
            b"sp" => self.shapes.push(false),
            b"ph" => {
                let body = attr(tag, b"type").as_deref() == Some("body");
                if let Some(shape) = self.shapes.last_mut() {
                    *shape = body;
                }
            }
            b"sld" => self.hidden = matches!(attr(tag, b"show").as_deref(), Some("0" | "false")),
            _ => {}
        }
    }

    fn close(&mut self, name: &[u8]) {
        match name {
            b"t" => self.text = false,
            b"r" => {
                if let Some(paragraph) = self.paragraphs.last_mut()
                    && let Some(run) = paragraph.run.take()
                    && !run.hidden
                {
                    paragraph.text.push_str(&run.text);
                }
            }
            b"p" => {
                if let Some(paragraph) = self.paragraphs.pop() {
                    self.end(paragraph);
                }
            }
            b"tc" => {
                if let Some(table) = self.tables.last_mut()
                    && let Some(cell) = table.cell.take()
                    && let Some(row) = table.rows.last_mut()
                {
                    row.push(cell);
                }
            }
            b"tbl" => {
                if let Some(table) = self.tables.pop() {
                    for line in table_lines(table) {
                        self.deliver(line);
                    }
                }
            }
            b"sp" => {
                self.shapes.pop();
            }
            _ => {}
        }
    }

    /// Adds `text` to the run open, or else to the paragraph.
    fn push(&mut self, text: &str) {
        let Some(paragraph) = self.paragraphs.last_mut() else {
            return;
        };
        match &mut paragraph.run {
            Some(run) => run.text.push_str(text),
            None => paragraph.text.push_str(text),
        }
    }

    /// Makes a finished paragraph a line, marked as a heading or list item.
    fn end(&mut self, paragraph: Paragraph) {
        let text = paragraph.text.trim();
        if text.is_empty() {
            return;
        }
        let line = match (paragraph.heading, paragraph.list) {
            (0, Some(depth)) => format!("{}- {text}", "  ".repeat(depth.min(8))),
            (0, None) => text.to_owned(),
            (level, _) => format!("{} {text}", "#".repeat(level)),
        };
        self.deliver(line);
    }

    /// Puts a finished line in the table cell open, or else among the lines.
    fn deliver(&mut self, line: String) {
        // A table in a cell is copied into it, escaped, and again into the
        // cell around that, so tables nested thousands deep would cost
        // gigabytes from a few hundred KB of XML were each copy not counted.
        self.bytes += line.len();
        if self.bytes > VIEW_BYTES {
            self.full = true;
            return;
        }
        if let Some(cell) = self.tables.last_mut().and_then(|t| t.cell.as_mut()) {
            if !cell.is_empty() {
                cell.push(' ');
            }
            cell.push_str(&line.replace('\n', " "));
            return;
        }
        if self.notes && !self.shapes.iter().any(|&body| body) {
            return;
        }
        self.lines.push(line);
    }
}

/// A table's rows as markdown, with a rule under the first.
fn table_lines(table: Table) -> Vec<String> {
    let mut lines = Vec::new();
    for row in table.rows.iter().filter(|row| !row.is_empty()) {
        let cells: Vec<String> = row.iter().map(|cell| cell.replace('|', "\\|")).collect();
        lines.push(format!("| {} |", cells.join(" | ")));
        if lines.len() == 1 {
            lines.push(format!("|{}", " --- |".repeat(cells.len())));
        }
    }
    lines
}

/// A sheet as the workbook lists it.
struct Listed {
    name: String,
    hidden: bool,
    link: Option<String>,
}

/// A workbook's sheets, each under a heading naming it and its size.
fn workbook(package: &mut Package, view: &mut View) -> Result<(), String> {
    let main = main_part(package, "xl/workbook.xml")?;
    let links = links_of(package, &main)?;
    let Some(bytes) = package.part(&main)? else {
        return Err(format!("it has no {main}"));
    };
    let (mut sheets, mut from_1904) = (Vec::new(), false);
    walk(&bytes[..], package.until, |node| {
        let Node::Open(tag) = node else {
            return true;
        };
        match local(tag) {
            b"workbookPr" => {
                from_1904 = matches!(attr(tag, b"date1904").as_deref(), Some("1" | "true"));
            }
            b"sheet" => sheets.push(Listed {
                name: attr(tag, b"name").unwrap_or_default(),
                hidden: attr(tag, b"state").is_some_and(|state| state != "visible"),
                link: link_id(tag),
            }),
            _ => {}
        }
        true
    })?;
    let mut cells = Cells {
        from_1904,
        ..Cells::default()
    };
    if let Some(part) = linked(&links, "sharedStrings")
        && let Some(bytes) = package.part(part)?
    {
        cells.strings = shared_strings(&bytes, package.until)?;
    }
    if let Some(part) = linked(&links, "styles")
        && let Some(bytes) = package.part(part)?
    {
        cells.dates = date_styles(&bytes, package.until)?;
    }
    let links = by_id(&links);
    for sheet in sheets {
        // A sheet the file lacks costs nothing to unpack, so only the clock
        // stops a list of them.
        if Instant::now() >= package.until {
            return Err(late());
        }
        let hidden = if sheet.hidden { "hidden, " } else { "" };
        let name = &sheet.name;
        let link = sheet.link.as_deref().and_then(|id| links.get(id));
        let Some(link) = link.filter(|link| link.kind.ends_with("/worksheet")) else {
            if !view.heading(&format!("## Sheet \"{name}\" ({hidden}not a worksheet)")) {
                break;
            }
            continue;
        };
        if package.zip.index_for_name(&link.target).is_none() {
            if !view.heading(&format!(
                "## Sheet \"{name}\" ({hidden}missing from the file)"
            )) {
                break;
            }
            continue;
        }
        let room = VIEW_BYTES.saturating_sub(view.text.len());
        let grid = grid(package, &link.target, &cells, room)?;
        if !show_sheet(view, name, sheet.hidden, &grid) {
            break;
        }
    }
    Ok(())
}

/// Adds the sheet `name` as `grid` read it, under a heading with its size;
/// false when no more of the workbook is read, the view being full or the
/// workbook unpacking to more than it may.
fn show_sheet(view: &mut View, name: &str, hidden: bool, grid: &Grid) -> bool {
    let hidden = if hidden { "hidden, " } else { "" };
    // A sheet not read to its end holds at least what was read of it.
    let least = if grid.cut || grid.full {
        "at least "
    } else {
        ""
    };
    let size = match grid.height {
        0 if least.is_empty() => "empty".to_owned(),
        height => format!("{least}{height} rows x {} cols", grid.width),
    };
    view.heading(&format!("## Sheet \"{name}\" ({hidden}{size})"));
    for line in &grid.lines {
        view.line(line);
    }
    if grid.more_rows {
        view.line(&format!("[Only the first {ROWS} rows are shown.]"));
    }
    if grid.more_columns {
        view.line(&format!(
            "[Only the first {COLUMNS} columns, A to IV, are shown.]"
        ));
    }
    if grid.full {
        view.full = true;
        return false;
    }
    if grid.cut {
        view.line(&format!(
            "[The rest is not read: the workbook unpacks to more than {} MB.]",
            UNPACKED_BYTES / (1024 * 1024)
        ));
        return false;
    }
    true
}

/// What a workbook's cells need to be shown as Excel shows them.
#[derive(Default)]
struct Cells {
    /// The text cells share, by index.
    strings: Vec<String>,
    /// Which cell styles, by index, show a number as a date or a time.
    dates: Vec<bool>,
    /// Whether its days count from 1904, as old Mac workbooks' do.
    from_1904: bool,
}

/// A cell being read: its type, its style, and its value as saved.
struct Cell {
    kind: Option<String>,
    style: usize,
    value: String,
}

impl Cells {
    /// Whether `cell` shows anything, as `show` would say, found without
    /// copying a shared string.
    fn holds(&self, cell: &Cell) -> bool {
        match cell.kind.as_deref() {
            Some("s") => cell
                .value
                .trim()
                .parse::<usize>()
                .ok()
                .and_then(|n| self.strings.get(n))
                .is_some_and(|text| !text.is_empty()),
            Some("str" | "inlineStr") => !cell.value.is_empty(),
            _ => !cell.value.trim().is_empty(),
        }
    }

    /// What `cell` shows, on one line.
    fn show(&self, cell: Cell) -> String {
        let value = cell.value.trim();
        let date = self.dates.get(cell.style).copied().unwrap_or(false);
        let shown = match cell.kind.as_deref() {
            Some("s") => value
                .parse::<usize>()
                .ok()
                .and_then(|n| self.strings.get(n))
                .cloned()
                .unwrap_or_default(),
            Some("b") if value == "1" => "TRUE".into(),
            Some("b") if value == "0" => "FALSE".into(),
            Some("n") | None if date => {
                serial_date(value, self.from_1904).unwrap_or_else(|| value.to_owned())
            }
            Some("str" | "inlineStr") => cell.value,
            _ => value.to_owned(),
        };
        shown.replace(['\t', '\n', '\r'], " ")
    }
}

/// A sheet read: its rows as lines, and what its heading and notes say.
#[derive(Default)]
struct Grid {
    /// The rows shown, as tab-separated values, from row 1.
    lines: Vec<String>,
    bytes: usize,
    /// The last row and column holding a value.
    height: u32,
    width: u32,
    /// Whether rows past `ROWS`, or columns past `COLUMNS`, hold values.
    more_rows: bool,
    more_columns: bool,
    /// Whether the lines stopped at the room the view had left.
    full: bool,
    /// Whether the sheet ran past what the workbook may unpack to.
    cut: bool,
}

/// Reads the sheet at `part`, its lines taking at most `room` bytes.
fn grid(package: &mut Package, part: &str, cells: &Cells, room: usize) -> Result<Grid, String> {
    let mut grid = Grid::default();
    let (mut row, mut column, mut row_bytes) = (0u32, 0u32, 0usize);
    let mut values: Vec<String> = Vec::new();
    let mut cell: Option<Cell> = None;
    // In a value, an inline string, its text, and its phonetic guide.
    let (mut value, mut inline, mut text, mut phonetic) = (false, false, false, 0usize);
    let cut = package.stream(part, |node| {
        match node {
            Node::Open(tag) => match local(tag) {
                b"row" => {
                    let number = attr(tag, b"r").and_then(|r| r.parse().ok());
                    row = number.unwrap_or(row.saturating_add(1));
                    (column, row_bytes) = (0, 0);
                    values.clear();
                }
                b"c" => {
                    let at = attr(tag, b"r").and_then(|at| column_of(&at));
                    column = at.unwrap_or(column.saturating_add(1));
                    cell = Some(Cell {
                        kind: attr(tag, b"t"),
                        style: attr(tag, b"s").and_then(|s| s.parse().ok()).unwrap_or(0),
                        value: String::new(),
                    });
                }
                b"v" => value = true,
                b"is" => inline = true,
                b"t" => text = true,
                b"rPh" => phonetic += 1,
                _ => {}
            },
            Node::Close(name) => match name {
                b"v" => value = false,
                b"is" => inline = false,
                b"t" => text = false,
                b"rPh" => phonetic = phonetic.saturating_sub(1),
                b"c" => {
                    // Shown only once it is known to be shown: each of a
                    // million cells past them can name one long string.
                    let Some(done) = cell.take().filter(|done| cells.holds(done)) else {
                        return true;
                    };
                    (grid.height, grid.width) = (grid.height.max(row), grid.width.max(column));
                    if row > ROWS {
                        grid.more_rows = true;
                        return true;
                    }
                    if column > COLUMNS {
                        grid.more_columns = true;
                        return true;
                    }
                    let shown = cells.show(done);
                    // Counted as it comes, since every cell can name the
                    // same long shared string.
                    row_bytes += shown.len() + 1;
                    if grid.bytes + row_bytes > room {
                        grid.full = true;
                        return false;
                    }
                    let at = column as usize - 1;
                    if values.len() <= at {
                        values.resize(at + 1, String::new());
                    }
                    values[at] = shown;
                }
                b"row" if row <= ROWS && !values.is_empty() => {
                    // A row with nothing in it is left out of the XML, so the
                    // lines of those before it are filled in empty.
                    while grid.lines.len() + 1 < row as usize {
                        grid.lines.push(String::new());
                        grid.bytes += 1;
                    }
                    let line = values.join("\t");
                    grid.bytes += line.len() + 1;
                    grid.lines.push(line);
                }
                _ => {}
            },
            Node::Text(chunk) => {
                if let Some(cell) = &mut cell
                    && (value || (inline && text && phonetic == 0))
                {
                    cell.value.push_str(chunk);
                }
            }
        }
        true
    })?;
    grid.cut = cut;
    Ok(grid)
}

/// The column, from 1, of a cell reference such as `AB12`.
fn column_of(reference: &str) -> Option<u32> {
    let column = reference
        .bytes()
        .take_while(u8::is_ascii_alphabetic)
        .fold(0u32, |n, letter| {
            let digit = u32::from(letter.to_ascii_uppercase() - b'A' + 1);
            n.saturating_mul(26).saturating_add(digit)
        });
    (column > 0).then_some(column)
}

/// The text of each string a workbook's cells share, without the phonetic
/// guides Japanese text carries.
fn shared_strings(bytes: &[u8], until: Instant) -> Result<Vec<String>, String> {
    let mut strings = Vec::new();
    let mut current = String::new();
    let (mut text, mut phonetic) = (false, 0usize);
    walk(bytes, until, |node| {
        match node {
            Node::Open(tag) => match local(tag) {
                b"si" => current.clear(),
                b"t" => text = true,
                b"rPh" => phonetic += 1,
                _ => {}
            },
            Node::Close(b"si") => strings.push(std::mem::take(&mut current)),
            Node::Close(b"t") => text = false,
            Node::Close(b"rPh") => phonetic = phonetic.saturating_sub(1),
            Node::Text(chunk) if text && phonetic == 0 => current.push_str(chunk),
            _ => {}
        }
        true
    })?;
    Ok(strings)
}

/// Which of a workbook's cell styles, by index, show a number as a date or
/// a time.
fn date_styles(bytes: &[u8], until: Instant) -> Result<Vec<bool>, String> {
    let mut formats = HashMap::new();
    let (mut styles, mut cell_styles) = (Vec::new(), false);
    walk(bytes, until, |node| {
        match node {
            Node::Open(tag) => match local(tag) {
                b"numFmt" => {
                    let id = attr(tag, b"numFmtId").and_then(|id| id.parse::<u32>().ok());
                    let code = attr(tag, b"formatCode").unwrap_or_default();
                    if let Some(id) = id {
                        formats.insert(id, shows_date(&code));
                    }
                }
                b"cellXfs" => cell_styles = true,
                b"xf" if cell_styles => {
                    let id = attr(tag, b"numFmtId").and_then(|id| id.parse::<u32>().ok());
                    let id = id.unwrap_or(0);
                    // The formats Excel has built in and saves by number only.
                    let built_in = matches!(id, 14..=22 | 27..=36 | 45..=47 | 50..=58);
                    styles.push(formats.get(&id).copied().unwrap_or(built_in));
                }
                _ => {}
            },
            Node::Close(b"cellXfs") => cell_styles = false,
            _ => {}
        }
        true
    })?;
    Ok(styles)
}

/// Whether a number format such as `yyyy-mm-dd` or `h:mm` shows a date or a
/// time: it has a letter of one outside quotes, brackets and escapes.
fn shows_date(code: &str) -> bool {
    let (mut chars, mut quoted, mut bracket) = (code.chars(), false, false);
    while let Some(c) = chars.next() {
        match c {
            '"' => quoted = !quoted,
            _ if quoted => {}
            // An escaped character, or the one whose width `_` leaves or
            // `*` fills with.
            '\\' | '_' | '*' => {
                chars.next();
            }
            '[' => bracket = true,
            ']' => bracket = false,
            _ if bracket => {}
            'd' | 'D' | 'm' | 'M' | 'y' | 'Y' | 'h' | 'H' | 's' | 'S' => return true,
            _ => {}
        }
    }
    false
}

/// An Excel date, a count of days with the time of day as its fraction, as
/// `2024-07-01`, `2024-07-01 18:00:00`, or under a day `18:00:00`.
fn serial_date(serial: &str, from_1904: bool) -> Option<String> {
    let serial: f64 = serial.parse().ok()?;
    // Past 9999-12-31, the last day Excel shows, a number is no date.
    if !(0.0..2_958_466.0).contains(&serial) {
        return None;
    }
    let mut days = serial.trunc() as i64;
    let mut seconds = ((serial - serial.trunc()) * 86_400.0).round() as u32;
    if seconds == 86_400 {
        (days, seconds) = (days + 1, 0);
    }
    let time = chrono::NaiveTime::from_num_seconds_from_midnight_opt(seconds, 0)?;
    if days == 0 {
        return Some(time.to_string());
    }
    // Excel counts a 29 February 1900 that never was, so its days before
    // March 1900 count from a day later.
    let base = match (from_1904, days < 61) {
        (true, _) => chrono::NaiveDate::from_ymd_opt(1904, 1, 1),
        (false, true) => chrono::NaiveDate::from_ymd_opt(1899, 12, 31),
        (false, false) => chrono::NaiveDate::from_ymd_opt(1899, 12, 30),
    };
    let date = base?.checked_add_signed(chrono::TimeDelta::try_days(days)?)?;
    Some(match seconds {
        0 => date.to_string(),
        _ => format!("{date} {time}"),
    })
}

/// A deck's slides in the order they show, each with its notes.
fn deck(package: &mut Package, view: &mut View) -> Result<(), String> {
    let main = main_part(package, "ppt/presentation.xml")?;
    let links = links_of(package, &main)?;
    let Some(bytes) = package.part(&main)? else {
        return Err(format!("it has no {main}"));
    };
    let mut order = Vec::new();
    walk(&bytes[..], package.until, |node| {
        if let Node::Open(tag) = node
            && local(tag) == b"sldId"
        {
            order.push(link_id(tag));
        }
        true
    })?;
    let links = by_id(&links);
    for (n, id) in order.iter().enumerate() {
        // A slide the file lacks costs nothing to unpack, so only the clock
        // stops a list of them.
        if Instant::now() >= package.until {
            return Err(late());
        }
        let Some(slide) = id.as_deref().and_then(|id| links.get(id)) else {
            continue;
        };
        let Some(bytes) = package.part(&slide.target)? else {
            continue;
        };
        let mut prose = Prose::default();
        walk(&bytes[..], package.until, |node| prose.take(node))?;
        let hidden = if prose.hidden { " (hidden)" } else { "" };
        if !view.heading(&format!("## Slide {}{hidden}", n + 1)) {
            break;
        }
        for line in &prose.lines {
            view.line(line);
        }
        if prose.full {
            view.full = true;
            break;
        }
        let slide_links = links_of(package, &slide.target)?;
        if let Some(part) = linked(&slide_links, "notesSlide")
            && let Some(bytes) = package.part(part)?
        {
            let mut notes = Prose {
                notes: true,
                ..Prose::default()
            };
            walk(&bytes[..], package.until, |node| notes.take(node))?;
            if !notes.lines.is_empty() {
                view.line("### Notes");
            }
            for line in &notes.lines {
                view.line(line);
            }
            if notes.full {
                view.full = true;
                break;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::io::Write;

    pub(crate) use super::odf::tests::odf;
    pub(crate) use super::pdf::tests::text_pdf;

    const W: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;
    const P: &str = r#"xmlns:p="http://schemas.openxmlformats.org/presentationml/2006/main" xmlns:a="http://schemas.openxmlformats.org/drawingml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;
    const X: &str = r#"xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships""#;

    /// A zip holding `parts`, each a name and its text, deflated as Office
    /// saves them.
    pub(crate) fn package(parts: &[(&str, &str)]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        for (name, text) in parts {
            zip.start_file(*name, options).unwrap();
            zip.write_all(text.as_bytes()).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    /// A relationships part with `links`, each an id, a type and a target.
    fn rels(links: &[(&str, &str, &str)]) -> String {
        let links: String = links
            .iter()
            .map(|(id, kind, target)| {
                format!(
                    r#"<Relationship Id="{id}" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/{kind}" Target="{target}"/>"#
                )
            })
            .collect();
        format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships">{links}</Relationships>"#
        )
    }

    /// A Word document whose body is `body`, with a styles part naming its
    /// first heading style in German.
    pub(crate) fn docx(body: &str) -> Vec<u8> {
        package(&[
            (
                "_rels/.rels",
                &rels(&[("rId1", "officeDocument", "word/document.xml")]),
            ),
            (
                "word/_rels/document.xml.rels",
                &rels(&[("rId1", "styles", "styles.xml")]),
            ),
            (
                "word/styles.xml",
                &format!(
                    r#"<w:styles {W}><w:style w:type="paragraph" w:styleId="berschrift1"><w:name w:val="heading 1"/></w:style></w:styles>"#
                ),
            ),
            (
                "word/document.xml",
                &format!(r#"<w:document {W}><w:body>{body}</w:body></w:document>"#),
            ),
        ])
    }

    /// A workbook with a sheet for each of `sheets`, a name and its XML, left
    /// out of the file when the XML is empty, and `strings` the text its
    /// cells share.
    fn book(sheets: &[(&str, &str)], strings: &str) -> Vec<u8> {
        let listed: String = (1..=sheets.len())
            .map(|n| {
                format!(
                    r#"<sheet name="{}" sheetId="{n}" r:id="rId{n}"/>"#,
                    sheets[n - 1].0
                )
            })
            .collect();
        let workbook = format!(r#"<workbook {X}><sheets>{listed}</sheets></workbook>"#);
        let ids: Vec<String> = (1..=sheets.len()).map(|n| format!("rId{n}")).collect();
        let targets: Vec<String> = (1..=sheets.len())
            .map(|n| format!("worksheets/sheet{n}.xml"))
            .collect();
        let mut links = vec![("rId0", "sharedStrings", "sharedStrings.xml")];
        links
            .extend((0..sheets.len()).map(|n| (ids[n].as_str(), "worksheet", targets[n].as_str())));
        let names: Vec<String> = targets
            .iter()
            .map(|target| format!("xl/{target}"))
            .collect();
        let (top, links) = (
            rels(&[("rId1", "officeDocument", "xl/workbook.xml")]),
            rels(&links),
        );
        let mut parts = vec![
            ("_rels/.rels", top.as_str()),
            ("xl/workbook.xml", workbook.as_str()),
            ("xl/_rels/workbook.xml.rels", links.as_str()),
            ("xl/sharedStrings.xml", strings),
        ];
        for (name, (_, xml)) in names.iter().zip(sheets) {
            if !xml.is_empty() {
                parts.push((name.as_str(), *xml));
            }
        }
        package(&parts)
    }

    /// The view of `bytes` saved as `name` in a folder of its own.
    pub(crate) fn view_of(name: &str, bytes: &[u8]) -> Result<Option<String>, String> {
        let dir = std::env::temp_dir().join(format!("letronna-docs-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        let view = view(&path);
        std::fs::remove_dir_all(&dir).unwrap();
        view
    }

    /// Rewrites what `zip` says its part `name` unpacks to, in its local
    /// header and in the central directory both.
    pub(crate) fn say_unpacked(zip: &mut [u8], name: &str, size: u32) {
        for at in 0..zip.len().saturating_sub(46) {
            let (name_at, size_at, length_at) = match &zip[at..at + 4] {
                b"PK\x03\x04" => (30, 22, 26),
                b"PK\x01\x02" => (46, 24, 28),
                _ => continue,
            };
            let length = u16::from_le_bytes([zip[at + length_at], zip[at + length_at + 1]]);
            let named = at + name_at..at + name_at + length as usize;
            if zip.get(named) == Some(name.as_bytes()) {
                zip[at + size_at..at + size_at + 4].copy_from_slice(&size.to_le_bytes());
            }
        }
    }

    #[test]
    fn a_docx_reads_with_headings_and_tables() {
        let run = |text: &str| format!(r#"<w:r><w:t xml:space="preserve">{text}</w:t></w:r>"#);
        let styled = |style: &str, text: &str| {
            format!(
                r#"<w:p><w:pPr><w:pStyle w:val="{style}"/></w:pPr>{}</w:p>"#,
                run(text)
            )
        };
        let cell = |text: &str| format!("<w:tc><w:tcPr/><w:p>{}</w:p></w:tc>", run(text));
        let boxed = "<w:txbxContent><w:p><w:r><w:t>Boxed</w:t></w:r></w:p></w:txbxContent>";
        let body = [
            styled("berschrift1", "Plan"),
            styled("Heading2", "Costs"),
            format!(
                r#"<w:p>{}<w:ins w:id="1">{}</w:ins><w:del w:id="2"><w:r><w:delText>late</w:delText></w:r></w:del><w:r><w:rPr><w:vanish/></w:rPr><w:t>hidden</w:t></w:r>{}<w:r><w:instrText> PAGE </w:instrText></w:r></w:p>"#,
                run("Rent is "),
                run("due"),
                run(" monthly &amp; on time&#x2E;")
            ),
            format!(
                r#"<w:p><w:pPr><w:numPr><w:ilvl w:val="1"/><w:numId w:val="3"/></w:numPr></w:pPr>{}</w:p>"#,
                run("Deposit")
            ),
            "<w:p><w:r><w:t></w:t></w:r></w:p>".into(),
            format!(
                "<w:tbl><w:tblPr/><w:tr>{}{}</w:tr><w:tr>{}{}</w:tr></w:tbl>",
                cell("Item"),
                cell("Cost"),
                cell("Desk | chair"),
                cell("40")
            ),
            // A text box comes twice: as a drawing, and as a copy for
            // programs that cannot show one.
            format!(
                "<w:p><w:r><mc:AlternateContent><mc:Choice Requires=\"wps\"><w:drawing>{boxed}</w:drawing></mc:Choice><mc:Fallback><w:pict>{boxed}</w:pict></mc:Fallback></mc:AlternateContent></w:r></w:p>"
            ),
        ]
        .concat();
        let view = view_of("plan.docx", &docx(&body)).unwrap().unwrap();
        assert_eq!(
            view,
            "# Plan\n## Costs\nRent is due monthly & on time.\n  - Deposit\n\
             | Item | Cost |\n| --- | --- |\n| Desk \\| chair | 40 |\nBoxed"
        );
    }

    #[test]
    fn an_xlsx_view_names_each_sheet_and_its_size() {
        let workbook = format!(
            r#"<workbook {X}><workbookPr/><sheets><sheet name="Budget" sheetId="1" r:id="rId1"/><sheet name="Old" sheetId="2" state="hidden" r:id="rId2"/><sheet name="Log" sheetId="3" r:id="rId3"/></sheets></workbook>"#
        );
        let links = rels(&[
            ("rId1", "worksheet", "worksheets/sheet1.xml"),
            ("rId2", "worksheet", "worksheets/sheet2.xml"),
            ("rId3", "worksheet", "/xl/worksheets/sheet3.xml"),
            ("rId4", "sharedStrings", "sharedStrings.xml"),
            ("rId5", "styles", "styles.xml"),
        ]);
        let strings = format!(
            r#"<sst {X}><si><t>Month</t></si><si><t>Amount</t></si><si><r><t xml:space="preserve">Rent </t></r><r><t>(flat)</t></r><rPh sb="0" eb="1"><t>reading</t></rPh></si></sst>"#
        );
        // Style 1 is a date by its own format, 2 a date and time built in,
        // 3 a number; the styles cells are based on do not count.
        let styles = format!(
            r#"<styleSheet {X}><numFmts count="1"><numFmt numFmtId="164" formatCode="yyyy\-mm\-dd;@"/></numFmts><cellStyleXfs count="1"><xf numFmtId="14"/></cellStyleXfs><cellXfs count="4"><xf numFmtId="0"/><xf numFmtId="164"/><xf numFmtId="22"/><xf numFmtId="4"/></cellXfs></styleSheet>"#
        );
        let budget = format!(
            r#"<worksheet {X}><dimension ref="A1:F4"/><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c><c r="B1" t="s"><v>1</v></c></row><row r="2"><c r="A2" s="1"><v>45474</v></c><c r="B2"><v>1250.5</v></c><c r="C2" s="2"><v>45474.75</v></c><c r="D2" s="3"><v>12</v></c></row><row r="4"><c r="A4" t="s"><v>2</v></c><c r="C4" t="b"><v>1</v></c><c r="D4" t="inlineStr"><is><t>a&#9;b</t></is></c><c r="E4" t="e"><v>#DIV/0!</v></c><c r="F4" t="str"><f>A1</f><v>Month</v></c><c r="G4" s="1"/></row></sheetData></worksheet>"#
        );
        let old = format!(
            r#"<worksheet {X}><sheetData><row r="1"><c r="B1"><v>7</v></c></row></sheetData></worksheet>"#
        );
        let rows: String = (1..=5001)
            .map(|n| format!(r#"<row r="{n}"><c r="A{n}"><v>{n}</v></c></row>"#))
            .collect();
        let log = format!(
            r#"<worksheet {X}><sheetData>{}<row r="5002"><c r="KN5002"><v>wide</v></c></row></sheetData></worksheet>"#,
            rows.replace(
                r#"<c r="A2"><v>2</v></c>"#,
                r#"<c r="A2"><v>2</v></c><c r="KN2"><v>x</v></c>"#
            )
        );
        let book = package(&[
            (
                "_rels/.rels",
                &rels(&[("rId1", "officeDocument", "xl/workbook.xml")]),
            ),
            ("xl/workbook.xml", &workbook),
            ("xl/_rels/workbook.xml.rels", &links),
            ("xl/sharedStrings.xml", &strings),
            ("xl/styles.xml", &styles),
            ("xl/worksheets/sheet1.xml", &budget),
            ("xl/worksheets/sheet2.xml", &old),
            ("xl/worksheets/sheet3.xml", &log),
        ]);
        let view = view_of("budget.xlsx", &book).unwrap().unwrap();
        let (start, log) = view.split_once("\n\n## Sheet \"Log\"").unwrap();
        assert_eq!(
            start,
            "## Sheet \"Budget\" (4 rows x 6 cols)\n\
             Month\tAmount\n\
             2024-07-01\t1250.5\t2024-07-01 18:00:00\t12\n\
             \n\
             Rent (flat)\t\tTRUE\ta b\t#DIV/0!\tMonth\n\
             \n\
             ## Sheet \"Old\" (hidden, 1 rows x 2 cols)\n\
             \t7"
        );
        // Rows past 5,000 and columns past IV are counted, not shown.
        let lines: Vec<&str> = log.lines().collect();
        assert_eq!(lines[0], " (5002 rows x 300 cols)");
        assert_eq!(&lines[1..3], ["1", "2"]);
        assert_eq!(lines[5000], "5000");
        assert_eq!(
            &lines[5001..],
            [
                "[Only the first 5000 rows are shown.]",
                "[Only the first 256 columns, A to IV, are shown.]"
            ]
        );
    }

    #[test]
    fn a_sheet_cut_at_the_unpack_limit_holds_at_least_what_was_read() {
        let rows: String = (1..=200)
            .map(|n| format!(r#"<row r="{n}"><c r="A{n}"><v>{n}</v></c></row>"#))
            .collect();
        let book = package(&[
            (
                "_rels/.rels",
                &rels(&[("rId1", "officeDocument", "xl/workbook.xml")]),
            ),
            (
                "xl/workbook.xml",
                &format!(
                    r#"<workbook {X}><sheets><sheet name="Data" sheetId="1" r:id="rId1"/></sheets></workbook>"#
                ),
            ),
            (
                "xl/_rels/workbook.xml.rels",
                &rels(&[("rId1", "worksheet", "worksheets/sheet1.xml")]),
            ),
            (
                "xl/worksheets/sheet1.xml",
                &format!("<worksheet {X}><sheetData>{rows}</sheetData></worksheet>"),
            ),
        ]);
        let dir = std::env::temp_dir().join(format!("letronna-docs-{}-cut", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cut.xlsx");
        std::fs::write(&path, book).unwrap();
        let mut package = Package::open(&path, Instant::now() + DEADLINE).unwrap();
        // Room for the workbook's own parts and the start of its sheet.
        package.left = 2_000;
        let mut view = View::default();
        workbook(&mut package, &mut view).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let view = view.finish();
        let lines: Vec<&str> = view.lines().collect();
        let shown = lines.len() - 2;
        assert!((10..200).contains(&shown), "{view}");
        assert!(
            lines[0].starts_with("## Sheet \"Data\" (at least ")
                && lines[0].ends_with(" rows x 1 cols)"),
            "{view}"
        );
        assert_eq!(lines[shown], shown.to_string());
        assert_eq!(
            lines[shown + 1],
            "[The rest is not read: the workbook unpacks to more than 50 MB.]"
        );
    }

    #[test]
    fn a_pptx_reads_slide_by_slide_with_notes() {
        let shape = |placeholder: &str, text: &str| {
            format!(
                r#"<p:sp><p:nvSpPr><p:cNvPr id="2" name="Shape"/><p:cNvSpPr/><p:nvPr>{placeholder}</p:nvPr></p:nvSpPr><p:txBody><a:bodyPr/><a:p>{text}</a:p></p:txBody></p:sp>"#
            )
        };
        let run = |text: &str| format!("<a:r><a:rPr lang=\"en-US\"/><a:t>{text}</a:t></a:r>");
        let tree = |shapes: &str| format!("<p:cSld><p:spTree>{shapes}</p:spTree></p:cSld>");
        let welcome = format!(
            "<p:sld {P}>{}</p:sld>",
            tree(
                &[
                    shape(r#"<p:ph type="title"/>"#, &run("Welcome")),
                    shape("", &format!("{}<a:br/>{}", run("Agenda:"), run("Budget"))),
                ]
                .concat()
            )
        );
        let notes = format!(
            "<p:notes {P}>{}</p:notes>",
            tree(
                &[
                    shape(r#"<p:ph type="sldImg"/>"#, ""),
                    shape(r#"<p:ph type="body" idx="1"/>"#, &run("Say hello")),
                    shape(
                        r#"<p:ph type="sldNum" idx="5"/>"#,
                        r#"<a:fld id="{1}" type="slidenum"><a:t>1</a:t></a:fld>"#
                    ),
                ]
                .concat()
            )
        );
        let cell =
            |text: &str| format!("<a:tc><a:txBody><a:p>{}</a:p></a:txBody></a:tc>", run(text));
        let table = format!(
            r#"<p:sld {P} show="0">{}</p:sld>"#,
            tree(&format!(
                "<p:graphicFrame><a:graphic><a:graphicData><a:tbl><a:tr>{}{}</a:tr></a:tbl></a:graphicData></a:graphic></p:graphicFrame>",
                cell("Q1"),
                cell("Q2")
            ))
        );
        // The file names run against the order the deck shows them in.
        let deck = package(&[
            (
                "_rels/.rels",
                &rels(&[("rId1", "officeDocument", "ppt/presentation.xml")]),
            ),
            (
                "ppt/presentation.xml",
                &format!(
                    r#"<p:presentation {P}><p:sldIdLst><p:sldId id="256" r:id="rId3"/><p:sldId id="257" r:id="rId2"/></p:sldIdLst></p:presentation>"#
                ),
            ),
            (
                "ppt/_rels/presentation.xml.rels",
                &rels(&[
                    ("rId1", "slideMaster", "slideMasters/slideMaster1.xml"),
                    ("rId2", "slide", "slides/slide1.xml"),
                    ("rId3", "slide", "slides/slide2.xml"),
                ]),
            ),
            ("ppt/slides/slide1.xml", &table),
            ("ppt/slides/slide2.xml", &welcome),
            (
                "ppt/slides/_rels/slide2.xml.rels",
                &rels(&[
                    ("rId1", "slideLayout", "../slideLayouts/slideLayout1.xml"),
                    ("rId2", "notesSlide", "../notesSlides/notesSlide1.xml"),
                ]),
            ),
            ("ppt/notesSlides/notesSlide1.xml", &notes),
        ]);
        let view = view_of("talk.pptx", &deck).unwrap().unwrap();
        assert_eq!(
            view,
            "## Slide 1\nWelcome\nAgenda:\nBudget\n### Notes\nSay hello\n\n\
             ## Slide 2 (hidden)\n| Q1 | Q2 |\n| --- | --- |"
        );
    }

    #[test]
    fn a_sheet_missing_from_the_file_is_named_and_the_rest_still_read() {
        let strings = format!(r#"<sst {X}><si><t>Kept</t></si></sst>"#);
        let kept = format!(
            r#"<worksheet {X}><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c></row></sheetData></worksheet>"#
        );
        let view = view_of(
            "gone.xlsx",
            &book(&[("Gone", ""), ("Kept", &kept)], &strings),
        );
        assert_eq!(
            view,
            Ok(Some(
                "## Sheet \"Gone\" (missing from the file)\n\n\
                 ## Sheet \"Kept\" (1 rows x 1 cols)\nKept"
                    .into()
            ))
        );
    }

    #[test]
    fn cells_past_the_rows_shown_are_counted_without_copying_their_text() {
        // 40,000 cells past row 5,000 naming one string of 4 MB: each one
        // copying it would take hundreds of GB of copying.
        let strings = format!(
            r#"<sst {X}><si><t>Head</t></si><si><t>{}</t></si></sst>"#,
            "x".repeat(4 << 20)
        );
        let far = r#"<c r="A6000" t="s"><v>1</v></c>"#.repeat(40_000);
        let sheet = format!(
            r#"<worksheet {X}><sheetData><row r="1"><c r="A1" t="s"><v>0</v></c></row><row r="6000">{far}</row></sheetData></worksheet>"#
        );
        let view = view_of("far.xlsx", &book(&[("Data", &sheet)], &strings));
        assert_eq!(
            view,
            Ok(Some(
                "## Sheet \"Data\" (6000 rows x 1 cols)\nHead\n\
                 [Only the first 5000 rows are shown.]"
                    .into()
            ))
        );
    }

    #[test]
    fn tables_nested_thousands_deep_stop_at_the_view_limit() {
        // Each table copies the one inside it, escaped, so uncounted 8,000
        // levels, 330 KB of XML, would copy hundreds of GB.
        let depth = 8_000;
        let body = format!(
            "{}<w:p><w:r><w:t>deep</w:t></w:r></w:p>{}",
            "<w:tbl><w:tr><w:tc>".repeat(depth),
            "</w:tc></w:tr></w:tbl>".repeat(depth)
        );
        assert_eq!(
            view_of("nested.docx", &docx(&body)),
            Ok(Some(
                "[The rest is not shown: the view stops at 16 MB.]".into()
            ))
        );
    }

    #[test]
    fn a_walk_past_its_deadline_stops_before_its_next_step() {
        let mut steps = 0;
        let walked = walk(&b"<a><b/></a>"[..], Instant::now(), |_| {
            steps += 1;
            true
        });
        assert_eq!(walked, Err("it took over 20 seconds".into()));
        assert_eq!(steps, 0);
    }

    #[test]
    fn a_deck_listing_slides_it_lacks_reads_well_within_its_deadline() {
        // 100,000 slides listed and none of them linked, beside 30,000 links:
        // matched one by one, that is three billion comparisons, past the
        // deadline; found by id, a moment.
        let listed: String = (0..100_000)
            .map(|n| format!(r#"<p:sldId id="{}" r:id="rS{n:05}"/>"#, 256 + n))
            .collect();
        let links: Vec<(String, String)> = (0..30_000)
            .map(|n| (format!("rL{n:05}"), format!("media/image{n}.png")))
            .collect();
        let links: Vec<(&str, &str, &str)> = links
            .iter()
            .map(|(id, target)| (id.as_str(), "image", target.as_str()))
            .collect();
        let deck = package(&[
            (
                "_rels/.rels",
                &rels(&[("rId1", "officeDocument", "ppt/presentation.xml")]),
            ),
            (
                "ppt/presentation.xml",
                &format!("<p:presentation {P}><p:sldIdLst>{listed}</p:sldIdLst></p:presentation>"),
            ),
            ("ppt/_rels/presentation.xml.rels", &rels(&links)),
        ]);
        assert_eq!(
            view_of("many.pptx", &deck),
            Ok(Some("[This document holds no text.]".into()))
        );
    }

    #[test]
    fn a_zip_listing_more_than_a_document_is_refused_before_it_is_read_whole() {
        // 70 files with names of 64,000 characters list in over 4 MB.
        let names: Vec<String> = (0..70)
            .map(|n| format!("{n:02}{}", "x".repeat(64_000)))
            .collect();
        let parts: Vec<(&str, &str)> = names.iter().map(|name| (name.as_str(), "")).collect();
        assert_eq!(
            view_of("long.docx", &package(&parts)),
            Err(
                "Could not read this .docx: the list of the files in it runs past 4 MB, \
                 longer than any document's."
                    .into()
            )
        );
    }

    #[test]
    fn templates_and_shows_read_as_their_kind() {
        for extension in [
            "dotx", "dotm", "xltx", "xltm", "potx", "potm", "ppsx", "ppsm",
        ] {
            let path = format!("a.{extension}");
            assert!(kind(Path::new(&path)).is_some(), "{extension}");
        }
        let letter = docx("<w:p><w:r><w:t>Dear customer</w:t></w:r></w:p>");
        assert_eq!(
            view_of("letter.dotx", &letter),
            Ok(Some("Dear customer".into()))
        );
    }

    #[test]
    fn a_zip_bomb_is_refused_before_inflating() {
        let mut bomb = docx("<w:p><w:r><w:t>boom</w:t></w:r></w:p>");
        // A bomb says what it unpacks to, gigabytes, which is never read.
        say_unpacked(&mut bomb, "word/document.xml", 3 << 30);
        assert_eq!(
            view_of("bomb.docx", &bomb),
            Err("Could not read this .docx: it unpacks to more than 50 MB.".into())
        );
        // One that says less than it holds is stopped as it unpacks.
        let mut small = docx("<w:p><w:r><w:t>boom</w:t></w:r></w:p>");
        say_unpacked(&mut small, "word/document.xml", 5);
        assert_eq!(
            view_of("small.docx", &small),
            Err(
                "Could not read this .docx: its word/document.xml unpacks to more than it says."
                    .into()
            )
        );
    }

    #[test]
    fn a_truncated_docx_says_why() {
        let whole = docx("<w:p><w:r><w:t>Cut</w:t></w:r></w:p>");
        let cut = view_of("cut.docx", &whole[..whole.len() / 2]).unwrap_err();
        assert!(
            cut.starts_with("Could not read this .docx: it is damaged or cut short ("),
            "{cut}"
        );
        // A file named as a document that is no zip reads as any other file.
        assert_eq!(view_of("notes.docx", b"plain words"), Ok(None));
        assert_eq!(view_of("notes.txt", &whole), Ok(None));
    }

    #[test]
    fn a_long_paragraph_is_split_at_spaces_so_no_line_is_cut() {
        let mut view = View::default();
        view.line(&"word ".repeat(450));
        let lines: Vec<&str> = view.text.lines().collect();
        assert_eq!(lines.len(), 3);
        assert!(
            lines
                .iter()
                .all(|line| line.chars().count() <= crate::tools::LINE_CHARS)
        );
        // Only the spaces the lines were split at are gone.
        assert_eq!(lines.concat().replace("word", "").len(), 450 - 2);
    }

    #[test]
    fn excel_dates_read_as_excel_shows_them() {
        assert_eq!(serial_date("1", false).as_deref(), Some("1900-01-01"));
        assert_eq!(serial_date("61", false).as_deref(), Some("1900-03-01"));
        assert_eq!(serial_date("0.5", false).as_deref(), Some("12:00:00"));
        assert_eq!(serial_date("1", true).as_deref(), Some("1904-01-02"));
        assert_eq!(serial_date("3000000", false), None);
        assert!(shows_date("[$-409]mmm d, yyyy") && shows_date("[h]:mm"));
        assert!(!shows_date("#,##0.00 \"days\"") && !shows_date("General"));
    }
}
