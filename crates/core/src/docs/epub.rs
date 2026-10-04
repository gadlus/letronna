//! EPUB books as read_file shows them: each chapter in the order the book
//! reads, under `## Chapter N` with its title from the table of contents
//! when it has one, the chapter's XHTML turned into text with its headings
//! as `#` marks and its lists and tables as markdown, as a Word document's
//! are. Scripts, styles and drawings are left out.
//!
//! A book is a zip of XML parts as an Office file is, and comes as often
//! from someone else, so it is read through the same package, within the
//! same limits and deadline. Each element open holds memory however few
//! bytes of XML it took, so none is followed deeper than `DEPTH`.

use super::{
    Node, Package, Table, VIEW_BYTES, View, attr, late, local, resolve, table_lines, walk,
};
use quick_xml::events::BytesStart;
use std::collections::HashMap;
use std::time::Instant;

/// The file every book has that names the package file listing its parts.
const CONTAINER: &str = "META-INF/container.xml";
/// How deep in each other elements are followed: a list ten levels down in
/// a table within a table is some forty deep, where XML nested thousands
/// deep is made to cost its reader, not to be read.
const DEPTH: usize = 100;
/// The most of a title a chapter's heading shows: a long title's worth, so
/// the heading stays one line of the view however long a contents entry is.
const TITLE_CHARS: usize = 200;
/// Elements whose text a reader does not see: the head with its title,
/// scripts and styles, drawings, and the reading over ruby text.
const LEFT_OUT: [&[u8]; 8] = [
    b"head",
    b"script",
    b"style",
    b"template",
    b"noscript",
    b"svg",
    b"rt",
    b"rp",
];
/// The ways a book may scramble its fonts so they are not copied out, which
/// leave its text readable, unlike the locks of a shop's copy protection.
const FONT_SCRAMBLES: [&str; 2] = [
    "http://www.idpf.org/2008/embedding",
    "http://ns.adobe.com/pdf/enc#RC",
];

/// A part the package file lists: where it is and what it holds.
struct Item {
    href: String,
    media: String,
    properties: String,
}

/// A book's chapters in the order it reads.
pub(super) fn book(package: &mut Package, view: &mut View) -> Result<(), String> {
    locked(package)?;
    let Some(bytes) = package.part(CONTAINER)? else {
        return Err(format!("it has no {CONTAINER}"));
    };
    let mut root = None;
    walk(&bytes[..], package.until, |node| {
        if let Node::Open(tag) = node
            && local(tag) == b"rootfile"
        {
            root = attr(tag, b"full-path");
        }
        root.is_none()
    })?;
    let Some(root) = root.map(|path| resolve("", &unescaped(&path))) else {
        return Err(format!("its {CONTAINER} names no package file"));
    };
    let Some(bytes) = package.part(&root)? else {
        return Err(format!("it has no {root}"));
    };
    let folder = folder_of(&root);
    let (mut items, mut ids, mut spine, mut ncx) = (Vec::new(), HashMap::new(), Vec::new(), None);
    walk(&bytes[..], package.until, |node| {
        let Node::Open(tag) = node else {
            return true;
        };
        match local(tag) {
            b"item" => {
                let id = attr(tag, b"id").unwrap_or_default();
                // The first of any that share an id, found once rather than
                // by matching each chapter against every part.
                ids.entry(id).or_insert(items.len());
                items.push(Item {
                    href: attr(tag, b"href").unwrap_or_default(),
                    media: attr(tag, b"media-type").unwrap_or_default(),
                    properties: attr(tag, b"properties").unwrap_or_default(),
                });
            }
            b"spine" => ncx = attr(tag, b"toc"),
            b"itemref" => spine.push(attr(tag, b"idref").unwrap_or_default()),
            _ => {}
        }
        true
    })?;
    let titles = titles(package, &items, ncx.and_then(|id| ids.get(&id)), folder)?;
    let mut n = 0;
    for id in &spine {
        // A chapter the file lacks costs nothing to unpack, so only the
        // clock stops a list of them.
        if Instant::now() >= package.until {
            return Err(late());
        }
        let Some(item) = ids.get(id).map(|&at| &items[at]) else {
            continue;
        };
        if !item.media.contains("html") {
            continue;
        }
        n += 1;
        let path = resolve(folder, &unescaped(&item.href));
        let heading = match titles.get(&path) {
            Some(title) => format!("## Chapter {n}: {title}"),
            None => format!("## Chapter {n}"),
        };
        let says = match package.zip.by_name(&path) {
            Ok(file) => Some(file.size()),
            Err(_) => None,
        };
        let Some(says) = says else {
            if !view.heading(&format!("{heading} (missing from the file)")) {
                break;
            }
            continue;
        };
        // Past the limit the chapters read so far are still shown.
        if says > package.left {
            view.heading(&heading);
            view.line(&format!(
                "[The rest is not read: the book unpacks to more than {} MB.]",
                super::UNPACKED_BYTES / (1024 * 1024)
            ));
            break;
        }
        let Some(bytes) = package.part(&path)? else {
            continue;
        };
        let mut page = Page::default();
        let walked = walk(&bytes[..], package.until, |node| page.take(node));
        if !view.heading(&heading) {
            break;
        }
        page.flush();
        for line in &page.lines {
            view.line(line);
        }
        match walked {
            Err(why) if why == late() => return Err(why),
            // Books are often hand-made: one broken chapter leaves the rest.
            Err(why) => {
                view.line(&format!("[The rest of this chapter is not read: {why}.]"));
            }
            Ok(()) => {}
        }
        if page.full {
            view.full = true;
            break;
        }
    }
    Ok(())
}

/// The folder a part such as `OEBPS/content.opf` is in.
fn folder_of(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(folder, _)| folder)
}

/// A link such as `Text/chapter%201.xhtml#start` as the name of the part it
/// is in: without its place in the part, its `%` escapes decoded.
fn unescaped(href: &str) -> String {
    let href = href.split('#').next().unwrap_or_default();
    let (bytes, mut out) = (href.as_bytes(), Vec::with_capacity(href.len()));
    let mut at = 0;
    while at < bytes.len() {
        let hex = bytes
            .get(at + 1..at + 3)
            .and_then(|hex| std::str::from_utf8(hex).ok())
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match (bytes[at], hex) {
            (b'%', Some(byte)) => {
                out.push(byte);
                at += 3;
            }
            (byte, _) => {
                out.push(byte);
                at += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// Refused when the book's encryption list locks more than its fonts, as a
/// shop's copy protection does, which would otherwise read as broken XML.
fn locked(package: &mut Package) -> Result<(), String> {
    let Some(bytes) = package.part("META-INF/encryption.xml")? else {
        return Ok(());
    };
    let mut locked = false;
    walk(&bytes[..], package.until, |node| {
        if let Node::Open(tag) = node
            && local(tag) == b"EncryptionMethod"
        {
            let how = attr(tag, b"Algorithm").unwrap_or_default();
            locked = !FONT_SCRAMBLES.contains(&how.as_str());
        }
        !locked
    })?;
    if locked {
        return Err(
            "it is locked with copy protection (DRM), which read_file cannot open; a copy \
             without it can be read"
                .into(),
        );
    }
    Ok(())
}

/// The title the table of contents gives each part it links to, by the
/// part's name: from EPUB 3's navigation document, else EPUB 2's NCX file.
fn titles(
    package: &mut Package,
    items: &[Item],
    ncx: Option<&usize>,
    folder: &str,
) -> Result<HashMap<String, String>, String> {
    let nav = items
        .iter()
        .find(|item| item.properties.split_whitespace().any(|p| p == "nav"));
    let (item, epub3) = match (nav, ncx.and_then(|&at| items.get(at))) {
        (Some(nav), _) => (nav, true),
        (None, Some(ncx)) => (ncx, false),
        (None, None) => return Ok(HashMap::new()),
    };
    let path = resolve(folder, &unescaped(&item.href));
    let Some(bytes) = package.part(&path)? else {
        return Ok(HashMap::new());
    };
    let mut contents = Contents {
        epub3,
        ..Contents::default()
    };
    // A table of contents that does not read leaves the chapters unnamed.
    if walk(&bytes[..], package.until, |node| contents.take(node)) == Err(late()) {
        return Err(late());
    }
    let at = folder_of(&path);
    let mut titles = HashMap::new();
    let entries = match contents.marked {
        true => contents.toc,
        false => contents.other,
    };
    for (href, title) in entries {
        let title: String = title.trim().chars().take(TITLE_CHARS).collect();
        if !title.is_empty() {
            titles
                .entry(resolve(at, &unescaped(&href)))
                .or_insert(title);
        }
    }
    Ok(titles)
}

/// The entries of a table of contents being read, each a link and its title.
#[derive(Default)]
struct Contents {
    epub3: bool,
    /// Those in a navigation list marked as the table of contents.
    toc: Vec<(String, String)>,
    /// Those in any other, used when none is marked: landmarks such as
    /// where the text begins name no chapter.
    other: Vec<(String, String)>,
    marked: bool,
    /// How deep the walk is, and where the navigation list open began and
    /// whether it is the table of contents.
    depth: usize,
    nav: Option<(usize, bool)>,
    /// The entry being read: its link, and its title so far.
    entry: Option<String>,
    title: String,
    spaced: bool,
    /// Whether the walk is in an NCX label's text.
    label: bool,
}

impl Contents {
    fn take(&mut self, node: Node) -> bool {
        match node {
            Node::Open(tag) => {
                self.depth += 1;
                let name = local(tag);
                match (self.epub3, name) {
                    (true, b"nav") if self.nav.is_none() => {
                        let toc = attr(tag, b"type")
                            .is_some_and(|kind| kind.split_whitespace().any(|k| k == "toc"));
                        self.nav = Some((self.depth, toc));
                        self.marked |= toc;
                    }
                    (true, b"a") if self.nav.is_some() => {
                        self.entry = attr(tag, b"href");
                        (self.title, self.spaced) = (String::new(), false);
                    }
                    (false, b"text") => {
                        self.label = true;
                        (self.title, self.spaced) = (String::new(), false);
                    }
                    (false, b"content") => {
                        if let Some(src) = attr(tag, b"src") {
                            self.other.push((src, std::mem::take(&mut self.title)));
                        }
                    }
                    _ => {}
                }
            }
            Node::Close(name) => {
                match (self.epub3, name) {
                    (true, b"a") => {
                        if let (Some(href), Some((_, toc))) = (self.entry.take(), self.nav) {
                            let title = std::mem::take(&mut self.title);
                            match toc {
                                true => self.toc.push((href, title)),
                                false => self.other.push((href, title)),
                            }
                        }
                    }
                    (true, b"nav") if self.nav.is_some_and(|(at, _)| at == self.depth) => {
                        self.nav = None;
                    }
                    (false, b"text") => self.label = false,
                    _ => {}
                }
                self.depth = self.depth.saturating_sub(1);
            }
            Node::Text(text) => {
                let reading = self.label || self.entry.is_some();
                // Kept to a few titles' worth: only the start is shown.
                if reading && self.title.len() < TITLE_CHARS * 4 {
                    soft(&mut self.title, &mut self.spaced, text);
                }
            }
        }
        true
    }
}

/// Adds the XML's own characters to `text`, where a run of spaces, tabs and
/// line ends shows as one space, as a browser shows them, and none at the
/// start.
fn soft(text: &mut String, spaced: &mut bool, chunk: &str) {
    for c in chunk.chars() {
        if c.is_whitespace() && c != '\u{a0}' {
            if !*spaced && !text.is_empty() {
                text.push(' ');
                *spaced = true;
            }
        } else {
            text.push(if c == '\u{a0}' { ' ' } else { c });
            *spaced = false;
        }
    }
}

/// A chapter's XHTML as lines: a line for each block, in the order the
/// blocks end, marked as a heading or list item, and tables as markdown.
#[derive(Default)]
struct Page {
    lines: Vec<String>,
    /// The text of the block open, and whether it ends in a space that the
    /// next ones join.
    text: String,
    spaced: bool,
    /// The heading level of the block open, 0 for none.
    heading: usize,
    /// For each list open, whether its item open has shown its mark.
    lists: Vec<bool>,
    tables: Vec<Table>,
    /// How deep the walk is in preformatted text, whose spaces all count.
    pre: usize,
    /// How deep the walk is, and in an element left out.
    depth: usize,
    skip: usize,
    /// Bytes of the lines finished, a table's counted again each time they
    /// go into a cell of the table around it.
    bytes: usize,
    /// Whether those, or the block open, passed `VIEW_BYTES`, which stops
    /// the walk.
    full: bool,
}

/// Elements that start and end a block of their own.
const BLOCKS: [&[u8]; 18] = [
    b"p",
    b"div",
    b"body",
    b"section",
    b"article",
    b"header",
    b"footer",
    b"aside",
    b"nav",
    b"main",
    b"figure",
    b"figcaption",
    b"blockquote",
    b"address",
    b"dl",
    b"dt",
    b"dd",
    b"caption",
];

impl Page {
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
            Node::Close(name) => {
                self.depth = self.depth.saturating_sub(1);
                self.close(&name.to_ascii_lowercase());
            }
            Node::Text(text) => {
                match self.pre {
                    0 => soft(&mut self.text, &mut self.spaced, text),
                    _ => {
                        self.text.push_str(text);
                        self.spaced = false;
                    }
                }
                // One block longer than the view would never be shown whole.
                self.full |= self.text.len() > VIEW_BYTES;
            }
        }
        !self.full
    }

    fn open(&mut self, tag: &BytesStart) {
        let name = local(tag).to_ascii_lowercase();
        if self.depth >= DEPTH || LEFT_OUT.contains(&name.as_slice()) {
            self.skip = 1;
            return;
        }
        self.depth += 1;
        match name.as_slice() {
            [b'h', level @ b'1'..=b'6'] => {
                self.flush();
                self.heading = usize::from(level - b'0');
            }
            b"pre" => {
                self.flush();
                self.pre += 1;
            }
            b"br" => {
                self.text.push('\n');
                self.spaced = true;
            }
            b"hr" => self.flush(),
            b"ul" | b"ol" => {
                self.flush();
                self.lists.push(false);
            }
            b"li" => {
                self.flush();
                if let Some(marked) = self.lists.last_mut() {
                    *marked = false;
                }
            }
            b"table" => {
                self.flush();
                self.tables.push(Table::default());
            }
            b"tr" => {
                self.flush();
                if let Some(table) = self.tables.last_mut() {
                    table.rows.push(Vec::new());
                }
            }
            b"td" | b"th" => {
                self.flush();
                if let Some(table) = self.tables.last_mut() {
                    table.cell = Some(String::new());
                }
            }
            name if BLOCKS.contains(&name) => self.flush(),
            _ => {}
        }
    }

    fn close(&mut self, name: &[u8]) {
        match name {
            [b'h', b'1'..=b'6'] => {
                self.flush();
                self.heading = 0;
            }
            b"pre" => {
                self.flush();
                self.pre = self.pre.saturating_sub(1);
            }
            b"ul" | b"ol" => {
                self.flush();
                self.lists.pop();
            }
            b"td" | b"th" => {
                self.flush();
                if let Some(table) = self.tables.last_mut()
                    && let Some(cell) = table.cell.take()
                    && let Some(row) = table.rows.last_mut()
                {
                    row.push(cell);
                }
            }
            b"table" => {
                self.flush();
                if let Some(table) = self.tables.pop() {
                    for line in table_lines(table) {
                        self.deliver(line);
                    }
                }
            }
            b"li" | b"tr" => self.flush(),
            name if BLOCKS.contains(&name) => self.flush(),
            _ => {}
        }
    }

    /// Ends the block open as a line, marked as a heading or list item.
    fn flush(&mut self) {
        self.spaced = false;
        let text = std::mem::take(&mut self.text);
        let text = match self.pre {
            0 => text.trim(),
            // Preformatted text keeps its indent.
            _ => text.trim_end().trim_start_matches(['\n', '\r']),
        };
        if text.is_empty() {
            return;
        }
        let depth = self.lists.len().saturating_sub(1).min(8);
        let line = match (self.heading, self.lists.last_mut()) {
            (0, Some(marked)) if !*marked => {
                *marked = true;
                format!("{}- {text}", "  ".repeat(depth))
            }
            // The item's later blocks, under its first.
            (0, Some(_)) => format!("{}  {text}", "  ".repeat(depth)),
            (0, None) => text.to_owned(),
            (level, _) => format!("{} {text}", "#".repeat(level)),
        };
        self.deliver(line);
    }

    /// Puts a finished line in the table cell open, or else among the lines.
    fn deliver(&mut self, line: String) {
        // Counted each time it is copied into the cell around it, as tables
        // nested in each other copy it again at every level.
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
        self.lines.push(line);
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::super::tests::{package, say_unpacked, view_of};
    use super::*;
    use std::time::Duration;

    /// A chapter whose body is `body`.
    pub(crate) fn xhtml(body: &str) -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><html xmlns="http://www.w3.org/1999/xhtml" xmlns:epub="http://www.idpf.org/2007/ops"><head><title>Not shown</title><style>p {{ color: red }}</style></head><body>{body}</body></html>"#
        )
    }

    /// An EPUB 3 book in OEBPS/ with `chapters`, each a file name and its
    /// XHTML, read in that order, and a navigation document listing `toc`,
    /// each a link and a title.
    pub(crate) fn epub(chapters: &[(&str, &str)], toc: &[(&str, &str)]) -> Vec<u8> {
        let items: String = chapters
            .iter()
            .enumerate()
            .map(|(n, (name, _))| {
                format!(r#"<item id="c{n}" href="{name}" media-type="application/xhtml+xml"/>"#)
            })
            .collect();
        let spine: String = (0..chapters.len())
            .map(|n| format!(r#"<itemref idref="c{n}"/>"#))
            .collect();
        let opf = format!(
            r#"<?xml version="1.0"?><package xmlns="http://www.idpf.org/2007/opf" version="3.0"><metadata/><manifest><item id="nav" href="nav.xhtml" media-type="application/xhtml+xml" properties="nav"/><item id="cover" href="cover.jpg" media-type="image/jpeg"/>{items}</manifest><spine><itemref idref="cover"/>{spine}</spine></package>"#
        );
        let links: String = toc
            .iter()
            .map(|(href, title)| format!(r#"<li><a href="{href}">{title}</a></li>"#))
            .collect();
        let nav = xhtml(&format!(
            r#"<nav epub:type="landmarks"><ol><li><a href="{}">Begin</a></li></ol></nav><nav epub:type="toc"><h1>Contents</h1><ol>{links}</ol></nav>"#,
            chapters.first().map_or("", |(name, _)| *name)
        ));
        let mut parts = vec![
            ("mimetype", "application/epub+zip".to_owned()),
            (
                "META-INF/container.xml",
                r#"<?xml version="1.0"?><container version="1.0" xmlns="urn:oasis:names:tc:opendocument:xmlns:container"><rootfiles><rootfile full-path="OEBPS/content.opf" media-type="application/oebps-package+xml"/></rootfiles></container>"#.to_owned(),
            ),
            ("OEBPS/content.opf", opf),
            ("OEBPS/nav.xhtml", nav),
        ];
        let names: Vec<String> = chapters
            .iter()
            .map(|(name, _)| format!("OEBPS/{}", unescaped(name)))
            .collect();
        for (name, (_, text)) in names.iter().zip(chapters) {
            if !text.is_empty() {
                parts.push((name.as_str(), (*text).to_owned()));
            }
        }
        let parts: Vec<(&str, &str)> = parts.iter().map(|(n, t)| (*n, t.as_str())).collect();
        package(&parts)
    }

    #[test]
    fn an_epub_reads_chapter_by_chapter_with_titles_from_its_contents() {
        let opening = xhtml(concat!(
            r#"<section epub:type="chapter"><h1>The   Start</h1><script>alert("no")</script>"#,
            "<p>It was a\n   dark&nbsp;night &amp; <em>cold</em>.<br/>Very cold.</p>",
            "<ul><li>Coat<ul><li>Wool</li></ul></li><li><p>Hat</p><p>red</p></li></ul>",
            "<table><tr><th>Item</th><th>Cost</th></tr><tr><td>Desk | chair</td><td>40</td></tr></table>",
            "<pre>  let x = 1;\n  let y = 2;</pre><svg><text>drawn</text></svg>",
            r#"<p>Said <ruby>漢<rp>(</rp><rt>kan</rt><rp>)</rp></ruby>.</p></section>"#,
        ));
        let second = xhtml("<h2>Later</h2><div>The end.</div>");
        let book = epub(
            &[
                ("text/one.xhtml", &opening),
                ("text/two%20b.xhtml", &second),
            ],
            &[
                ("text/one.xhtml#start", " The\n Start "),
                ("text/one.xhtml#middle", "Middle"),
                ("text/two%20b.xhtml", "Two"),
            ],
        );
        let view = view_of("story.epub", &book).unwrap().unwrap();
        assert_eq!(
            view,
            "## Chapter 1: The Start\n# The Start\nIt was a dark night & cold.\nVery cold.\n\
             - Coat\n  - Wool\n- Hat\n  red\n| Item | Cost |\n| --- | --- |\n| Desk \\| chair | 40 |\n\
             \x20 let x = 1;\n  let y = 2;\nSaid 漢.\n\n\
             ## Chapter 2: Two\n## Later\nThe end."
        );
        let lines: Vec<&[u8]> = view.lines().map(str::as_bytes).collect();
        let places = super::super::places(std::path::Path::new("story.epub"), &lines);
        assert_eq!(places, [0, 15]);
    }

    #[test]
    fn an_epub2_takes_its_titles_from_its_ncx() {
        let opf = r#"<package xmlns="http://www.idpf.org/2007/opf" version="2.0"><manifest><item id="ncx" href="toc.ncx" media-type="application/x-dtbncx+xml"/><item id="a" href="a.html" media-type="application/xhtml+xml"/><item id="b" href="b.html" media-type="application/xhtml+xml"/></manifest><spine toc="ncx"><itemref idref="a"/><itemref idref="b" linear="no"/><itemref idref="gone"/></spine></package>"#;
        let ncx = r#"<ncx xmlns="http://www.daisy.org/z3986/2005/ncx/"><docTitle><text>Book</text></docTitle><navMap><navPoint id="p1"><navLabel><text>One</text></navLabel><content src="a.html"/><navPoint id="p2"><navLabel><text>One, part two</text></navLabel><content src="a.html#two"/></navPoint></navPoint></navMap></ncx>"#;
        let book = package(&[
            ("mimetype", "application/epub+zip"),
            (
                "META-INF/container.xml",
                r#"<container><rootfiles><rootfile full-path="content.opf"/></rootfiles></container>"#,
            ),
            ("content.opf", opf),
            ("toc.ncx", ncx),
            ("a.html", &xhtml("<p>First</p>")),
            ("b.html", &xhtml("<p>Notes</p>")),
        ]);
        assert_eq!(
            view_of("old.epub", &book),
            Ok(Some(
                "## Chapter 1: One\nFirst\n\n## Chapter 2\nNotes".into()
            ))
        );
    }

    #[test]
    fn a_book_missing_or_breaking_its_parts_says_what_and_reads_the_rest() {
        let none = package(&[("mimetype", "application/epub+zip")]);
        assert_eq!(
            view_of("none.epub", &none),
            Err("Could not read this .epub: it has no META-INF/container.xml.".into())
        );
        let unnamed = package(&[("META-INF/container.xml", "<container/>")]);
        assert_eq!(
            view_of("unnamed.epub", &unnamed),
            Err(
                "Could not read this .epub: its META-INF/container.xml names no package file."
                    .into()
            )
        );
        let mut book = epub(&[("a.xhtml", "<p>x</p>")], &[]);
        say_unpacked(&mut book, "OEBPS/content.opf", 3 << 30);
        assert_eq!(
            view_of("bomb.epub", &book),
            Err("Could not read this .epub: it unpacks to more than 50 MB.".into())
        );
        // A chapter missing, one broken, one in HTML that is not XML: the
        // others are still read.
        let book = epub(
            &[
                ("gone.xhtml", ""),
                ("broken.xhtml", &xhtml("<p>Kept</p><p>cut</div>")),
                ("html.xhtml", "<html><body><p>One<br>two</p></body></html>"),
                ("fine.xhtml", &xhtml("<p>Fine</p>")),
            ],
            &[],
        );
        let view = view_of("broken.epub", &book).unwrap().unwrap();
        let broken = |expected: &str, found: &str| {
            format!(
                "[The rest of this chapter is not read: its XML is broken (ill-formed document: \
                 expected `</{expected}>`, but `</{found}>` was found).]"
            )
        };
        assert_eq!(
            view,
            format!(
                "## Chapter 1 (missing from the file)\n\n\
                 ## Chapter 2\nKept\ncut\n{}\n\n\
                 ## Chapter 3\nOne\ntwo\n{}\n\n\
                 ## Chapter 4\nFine",
                broken("p", "div"),
                broken("br", "p")
            )
        );
        // Cut short, or no zip at all.
        let whole = epub(&[("a.xhtml", &xhtml("<p>a</p>"))], &[]);
        let cut = view_of("cut.epub", &whole[..whole.len() / 2]).unwrap_err();
        assert!(
            cut.starts_with("Could not read this .epub: it is damaged or cut short ("),
            "{cut}"
        );
        assert_eq!(view_of("plain.epub", b"plain words"), Ok(None));
    }

    #[test]
    fn a_book_locked_by_a_shop_says_so_and_one_with_scrambled_fonts_reads() {
        let encryption = |how: &str| {
            format!(
                r#"<encryption xmlns="urn:oasis:names:tc:opendocument:xmlns:container" xmlns:enc="http://www.w3.org/2001/04/xmlenc#"><enc:EncryptedData><enc:EncryptionMethod Algorithm="{how}"/><enc:CipherData><enc:CipherReference URI="OEBPS/a.xhtml"/></enc:CipherData></enc:EncryptedData></encryption>"#
            )
        };
        let with = |how: &str| {
            let book = epub(&[("a.xhtml", &xhtml("<p>Text</p>"))], &[]);
            let mut zip = zip::ZipArchive::new(std::io::Cursor::new(book)).unwrap();
            let mut parts = Vec::new();
            for n in 0..zip.len() {
                let mut file = zip.by_index(n).unwrap();
                let mut text = String::new();
                std::io::Read::read_to_string(&mut file, &mut text).unwrap();
                parts.push((file.name().to_owned(), text));
            }
            parts.push(("META-INF/encryption.xml".into(), encryption(how)));
            let parts: Vec<(&str, &str)> = parts
                .iter()
                .map(|(n, t)| (n.as_str(), t.as_str()))
                .collect();
            package(&parts)
        };
        assert_eq!(
            view_of(
                "bought.epub",
                &with("http://www.w3.org/2001/04/xmlenc#aes128-cbc")
            ),
            Err(
                "Could not read this .epub: it is locked with copy protection (DRM), which \
                 read_file cannot open; a copy without it can be read."
                    .into()
            )
        );
        assert_eq!(
            view_of("fonts.epub", &with("http://www.idpf.org/2008/embedding")),
            Ok(Some("## Chapter 1\nText".into()))
        );
    }

    #[test]
    fn a_chapter_past_the_unpack_limit_ends_the_book_with_a_note() {
        let mut book = epub(
            &[
                ("a.xhtml", &xhtml("<p>Read</p>")),
                ("b.xhtml", &xhtml("<p>Bomb</p>")),
            ],
            &[],
        );
        say_unpacked(&mut book, "OEBPS/b.xhtml", 3 << 30);
        assert_eq!(
            view_of("huge.epub", &book),
            Ok(Some(
                "## Chapter 1\nRead\n\n## Chapter 2\n\
                 [The rest is not read: the book unpacks to more than 50 MB.]"
                    .into()
            ))
        );
        let mut small = epub(&[("a.xhtml", &xhtml("<p>Small</p>"))], &[]);
        say_unpacked(&mut small, "OEBPS/a.xhtml", 5);
        assert_eq!(
            view_of("small.epub", &small),
            Err(
                "Could not read this .epub: its OEBPS/a.xhtml unpacks to more than it says.".into()
            )
        );
    }

    #[test]
    fn a_spine_listing_one_chapter_over_and_over_stops_at_the_limits() {
        // 200,000 chapters the book lacks, matched one by one against its
        // parts, then 200,000 readings of one chapter of 1 MB: 200 GB, were
        // each unpacked.
        let chapter = xhtml(&format!("<p>Again</p><!--{}-->", "x".repeat(1 << 20)));
        let opf = format!(
            r#"<package><manifest><item id="a" href="a.xhtml" media-type="application/xhtml+xml"/></manifest><spine>{}{}</spine></package>"#,
            r#"<itemref idref="nowhere"/>"#.repeat(200_000),
            r#"<itemref idref="a"/>"#.repeat(200_000),
        );
        let book = package(&[
            (
                "META-INF/container.xml",
                r#"<container><rootfile full-path="content.opf"/></container>"#,
            ),
            ("content.opf", &opf),
            ("a.xhtml", &chapter),
        ]);
        let started = Instant::now();
        let view = view_of("again.epub", &book).unwrap().unwrap();
        assert!(started.elapsed() < Duration::from_secs(15));
        // Read until what is left to unpack, after the package file's 10 MB,
        // is less than the chapter.
        assert_eq!(view.matches("\nAgain\n").count(), 41, "{view}");
        assert!(
            view.ends_with(
                "## Chapter 42\n[The rest is not read: the book unpacks to more than 50 MB.]"
            ),
            "{}",
            &view[view.len() - 200..]
        );
    }

    #[test]
    fn elements_nested_thousands_deep_are_read_no_deeper_than_the_limit() {
        let depth = 200_000;
        for (open, close) in [
            ("<div>", "</div>"),
            ("<ul><li>", "</li></ul>"),
            ("<table><tr><td>", "</td></tr></table>"),
        ] {
            let body = format!(
                "<p>top</p>{}deep{}",
                open.repeat(depth / 3),
                close.repeat(depth / 3)
            );
            let started = Instant::now();
            let view = view_of("deep.epub", &epub(&[("a.xhtml", &xhtml(&body))], &[]))
                .unwrap()
                .unwrap();
            assert!(started.elapsed() < Duration::from_secs(10));
            assert!(
                view.starts_with("## Chapter 1\ntop"),
                "{}",
                &view[..view.len().min(200)]
            );
            assert!(!view.contains("deep"));
        }
    }

    #[test]
    fn a_long_chapter_stops_at_the_view_limit() {
        // 30 MB of text in 40 KB paragraphs, within what a book may unpack to.
        let paragraph = format!("<p>{}</p>", "z".repeat(40_000));
        let body = paragraph.repeat(750);
        let view = view_of("long.epub", &epub(&[("a.xhtml", &xhtml(&body))], &[]))
            .unwrap()
            .unwrap();
        assert!(
            (15_000_000..=VIEW_BYTES + 100).contains(&view.len()),
            "{}",
            view.len()
        );
        assert!(view.ends_with("\n\n[The rest is not shown: the view stops at 16 MB.]"));
    }

    #[test]
    fn links_name_their_parts_with_escapes_decoded() {
        assert_eq!(
            unescaped("Text/chapter%201.xhtml#p3"),
            "Text/chapter 1.xhtml"
        );
        assert_eq!(unescaped("caf%C3%A9.xhtml"), "café.xhtml");
        assert_eq!(unescaped("100%.xhtml%2"), "100%.xhtml%2");
        assert_eq!(
            resolve("OEBPS/text", &unescaped("../images/../b.xhtml")),
            "OEBPS/b.xhtml"
        );
    }
}
