//! OpenDocument text, spreadsheets and presentations (.odt, .ods, .odp and
//! their templates) as read_file shows them, each in the view of its Office
//! kin: headings as `#` marks with lists and tables as markdown, sheets as
//! rows of the values they last showed, slides with their notes.
//!
//! Each is a zip of XML as an Office file is, read through the same package
//! within the same limits and deadline. The whole document is one part,
//! content.xml, so a spreadsheet is walked as it unpacks, as a workbook's
//! sheet is. Its elements are followed no deeper than `DEPTH`, since every
//! element open costs the reader memory, however few bytes of XML it took.

use super::{
    COLUMNS, Grid, Node, Package, ROWS, Table, VIEW_BYTES, View, attr, local, show_sheet,
    table_lines, walk,
};
use quick_xml::events::BytesStart;
use std::collections::HashSet;

/// The part that holds the whole document.
const CONTENT: &str = "content.xml";
/// How deep in each other elements are followed: a list ten levels down in
/// a table within a table is some forty deep, where XML nested thousands
/// deep is made to cost its reader, not to be read.
const DEPTH: usize = 100;
/// Elements whose text a reader does not see: deleted text kept to undo,
/// comments and footnotes, as a Word document's are not shown, and the
/// reading over ruby text.
const LEFT_OUT: [&[u8]; 4] = [b"tracked-changes", b"annotation", b"note", b"ruby-text"];

/// An OpenDocument text's paragraphs, headings, lists and tables.
pub(super) fn text(package: &mut Package, view: &mut View) -> Result<(), String> {
    let bytes = content(package)?;
    let (mut steps, mut flow) = (Steps::default(), Flow::default());
    walk(&bytes[..], package.until, |node| {
        !steps.pass(&node) || flow.take(node, &steps.hidden.text)
    })?;
    for line in &flow.lines {
        if !view.line(line) {
            break;
        }
    }
    view.full |= flow.full;
    Ok(())
}

/// A slide being read: its text, its notes, and where the walk is in it.
#[derive(Default)]
struct Slide {
    hidden: bool,
    body: Flow,
    notes: Flow,
    /// How deep the walk is in the slide.
    depth: usize,
    /// How deep its notes opened, while the walk is in them.
    notes_at: Option<usize>,
}

/// An OpenDocument presentation's slides in the order they show, each with
/// its notes.
pub(super) fn presentation(package: &mut Package, view: &mut View) -> Result<(), String> {
    let bytes = content(package)?;
    let mut steps = Steps::default();
    let (mut slide, mut shown) = (None::<Slide>, 0);
    walk(&bytes[..], package.until, |node| {
        if !steps.pass(&node) {
            return true;
        }
        let Some(open) = &mut slide else {
            if let Node::Open(tag) = node
                && local(tag) == b"page"
            {
                let style = attr(tag, b"style-name").unwrap_or_default();
                slide = Some(Slide {
                    hidden: steps.hidden.slides.contains(&style),
                    ..Slide::default()
                });
            }
            return true;
        };
        match node {
            Node::Open(tag) => {
                open.depth += 1;
                if open.notes_at.is_none() && local(tag) == b"notes" {
                    open.notes_at = Some(open.depth);
                    return true;
                }
            }
            Node::Close(_) if open.depth == 0 => {
                shown += 1;
                return slide
                    .take()
                    .is_some_and(|done| show_slide(view, shown, &done));
            }
            Node::Close(_) => {
                let depth = open.depth;
                open.depth -= 1;
                if open.notes_at == Some(depth) {
                    open.notes_at = None;
                    return true;
                }
            }
            Node::Text(_) => {}
        }
        let flow = match open.notes_at {
            Some(_) => &mut open.notes,
            None => &mut open.body,
        };
        if flow.take(node, &steps.hidden.text) {
            return true;
        }
        // Full: what was read of the slide is shown, and no more.
        shown += 1;
        if let Some(done) = slide.take() {
            show_slide(view, shown, &done);
        }
        false
    })?;
    Ok(())
}

/// Adds slide `n` and its notes to `view`; false once no more is read.
fn show_slide(view: &mut View, n: usize, slide: &Slide) -> bool {
    let hidden = if slide.hidden { " (hidden)" } else { "" };
    if !view.heading(&format!("## Slide {n}{hidden}")) {
        return false;
    }
    for line in &slide.body.lines {
        view.line(line);
    }
    if !slide.notes.lines.is_empty() {
        view.line("### Notes");
    }
    for line in &slide.notes.lines {
        view.line(line);
    }
    view.full |= slide.body.full || slide.notes.full;
    !view.full
}

/// An OpenDocument spreadsheet's sheets, each under a heading naming it and
/// its size, as a workbook's are.
pub(super) fn spreadsheet(package: &mut Package, view: &mut View) -> Result<(), String> {
    locked(package)?;
    if package.zip.index_for_name(CONTENT).is_none() {
        return Err(format!("it has no {CONTENT}"));
    }
    let mut steps = Steps::default();
    let mut sheet = None::<Sheet>;
    // Every sheet is in the one part, so it is walked once as it unpacks,
    // each sheet shown as it ends.
    let cut = package.stream(CONTENT, |node| {
        if !steps.pass(&node) {
            return true;
        }
        let Some(open) = &mut sheet else {
            if let Node::Open(tag) = node
                && local(tag) == b"table"
            {
                let style = attr(tag, b"style-name").unwrap_or_default();
                sheet = Some(Sheet {
                    name: attr(tag, b"name").unwrap_or_default(),
                    hidden: steps.hidden.sheets.contains(&style),
                    room: VIEW_BYTES.saturating_sub(view.text.len()),
                    ..Sheet::default()
                });
            }
            return true;
        };
        if open.take(node) {
            return true;
        }
        let Some(done) = sheet.take() else {
            return true;
        };
        show_sheet(view, &done.name, done.hidden, &done.grid) && !done.grid.full
    })?;
    // Stopped inside a sheet: at the unpack limit, or with the view full.
    if let Some(mut open) = sheet {
        open.grid.cut = cut;
        show_sheet(view, &open.name, open.hidden, &open.grid);
    }
    Ok(())
}

/// A sheet being read, and where the walk is in it.
#[derive(Default)]
struct Sheet {
    name: String,
    hidden: bool,
    grid: Grid,
    /// How many bytes its lines may take.
    room: usize,
    /// How deep the walk is in the sheet.
    depth: usize,
    /// The rows met so far, a row repeated counted as often as it is.
    rows: u32,
    /// The first and last row the row open stands for.
    row: (u32, u32),
    /// The columns met so far in the row open.
    column: u32,
    values: Vec<String>,
    row_bytes: usize,
    cell: Option<Cell>,
}

/// A cell being read: what it shows, and how often it is repeated.
#[derive(Default)]
struct Cell {
    text: String,
    /// Whether the text ends in a space that the next ones join.
    spaced: bool,
    /// Its value as saved, shown when it has no text, as some programs save
    /// a cell.
    value: Option<String>,
    repeat: u32,
    /// How deep the walk is in the cell.
    depth: usize,
    /// How deep the walk is in an element whose text is not the cell's.
    skip: usize,
}

impl Sheet {
    /// Takes the next step of the walk; false once the sheet has ended or
    /// its lines are full.
    fn take(&mut self, node: Node) -> bool {
        if let Some(cell) = &mut self.cell {
            if cell.skip > 0 || cell.depth > 0 {
                cell.take(node);
                return true;
            }
            match node {
                Node::Open(tag) => cell.take(Node::Open(tag)),
                Node::Text(_) => {}
                Node::Close(_) => {
                    if let Some(done) = self.cell.take() {
                        self.end_cell(done);
                    }
                }
            }
            return !self.grid.full;
        }
        match node {
            Node::Open(tag) => {
                self.depth += 1;
                match local(tag) {
                    b"table-row" => {
                        let repeat = repeated(tag, b"number-rows-repeated");
                        self.row = (
                            self.rows.saturating_add(1),
                            self.rows.saturating_add(repeat),
                        );
                        self.rows = self.row.1;
                        (self.column, self.row_bytes) = (0, 0);
                        self.values.clear();
                    }
                    b"table-cell" | b"covered-table-cell" => {
                        self.cell = Some(Cell {
                            value: shown_value(tag),
                            repeat: repeated(tag, b"number-columns-repeated"),
                            ..Cell::default()
                        });
                        // A cell's own close ends it, so its depth is not counted.
                        self.depth -= 1;
                    }
                    _ => {}
                }
            }
            Node::Close(_) if self.depth == 0 => return false,
            Node::Close(name) => {
                self.depth -= 1;
                if name == b"table-row" {
                    self.end_row();
                }
            }
            Node::Text(_) => {}
        }
        !self.grid.full
    }

    /// Puts a finished cell in the row, once for each column it repeats in.
    fn end_cell(&mut self, cell: Cell) {
        let first = self.column.saturating_add(1);
        self.column = self.column.saturating_add(cell.repeat);
        let mut shown = cell.text.trim_end().to_owned();
        if shown.is_empty() {
            shown = cell.value.unwrap_or_default();
        }
        // A cell shown nowhere is left out, however often it repeats: a
        // sheet saves its empty rows and columns to the last of a million.
        if shown.is_empty() {
            return;
        }
        let grid = &mut self.grid;
        (grid.height, grid.width) = (grid.height.max(self.row.1), grid.width.max(self.column));
        if self.row.0 > ROWS {
            grid.more_rows = true;
            return;
        }
        if self.column > COLUMNS {
            grid.more_columns = true;
        }
        for column in first..=self.column.min(COLUMNS) {
            // Counted as each copy is made: one long text can repeat across
            // every column.
            self.row_bytes += shown.len() + 1;
            if grid.bytes + self.row_bytes > self.room {
                grid.full = true;
                return;
            }
            let at = column as usize - 1;
            if self.values.len() <= at {
                self.values.resize(at + 1, String::new());
            }
            self.values[at] = shown.clone();
        }
    }

    /// Adds the row ended as lines, once for each row it repeats as.
    fn end_row(&mut self) {
        if self.values.is_empty() || self.grid.full {
            return;
        }
        let (first, last) = self.row;
        if last > ROWS {
            self.grid.more_rows = true;
        }
        let line = self.values.join("\t");
        for row in first..=last.min(ROWS) {
            let grid = &mut self.grid;
            // A row with nothing in it is left out of the lines, so those
            // before it are filled in empty, as a workbook's are.
            let gap = (row as usize - 1).saturating_sub(grid.lines.len());
            if grid.bytes + gap + line.len() + 1 > self.room {
                grid.full = true;
                return;
            }
            grid.lines.extend(std::iter::repeat_n(String::new(), gap));
            grid.bytes += gap + line.len() + 1;
            grid.lines.push(line.clone());
        }
    }
}

impl Cell {
    /// Takes a step inside the cell: only the text of its own paragraphs
    /// counts, not a comment, a shape or a text box on it.
    fn take(&mut self, node: Node) {
        if self.skip > 0 {
            match node {
                Node::Open(_) => self.skip += 1,
                Node::Close(_) => self.skip -= 1,
                Node::Text(_) => {}
            }
            return;
        }
        match node {
            Node::Open(tag) => {
                let name = local(tag);
                let paragraph = matches!(name, b"p" | b"h");
                // A paragraph within a paragraph is in a shape or a box.
                if paragraph != (self.depth == 0) || LEFT_OUT.contains(&name) || alternative(tag) {
                    self.skip = 1;
                    return;
                }
                self.depth += 1;
                if paragraph && !self.text.is_empty() {
                    soft(&mut self.text, &mut self.spaced, " ");
                }
                if let Some(spaces) = spaces(tag)
                    && self.text.len() <= VIEW_BYTES
                {
                    hard(
                        &mut self.text,
                        &mut self.spaced,
                        &spaces.replace(['\t', '\n'], " "),
                    );
                }
            }
            Node::Close(_) => self.depth = self.depth.saturating_sub(1),
            Node::Text(text) if self.text.len() <= VIEW_BYTES => {
                soft(&mut self.text, &mut self.spaced, text);
            }
            Node::Text(_) => {}
        }
    }
}

/// How many times `tag` repeats, by its attribute `name`: at least once.
fn repeated(tag: &BytesStart, name: &[u8]) -> u32 {
    attr(tag, name)
        .and_then(|n| n.trim().parse().ok())
        .unwrap_or(1)
        .max(1)
}

/// The value a cell saves beside the text it shows, as it would show it.
fn shown_value(tag: &BytesStart) -> Option<String> {
    if let Some(truth) = attr(tag, b"boolean-value") {
        return Some(if truth == "true" { "TRUE" } else { "FALSE" }.into());
    }
    [&b"value"[..], b"date-value", b"time-value", b"string-value"]
        .iter()
        .find_map(|name| attr(tag, name))
        .map(|value| value.replace(['\t', '\n', '\r'], " "))
}

/// Where a walk of content.xml is: the styles before the body, which say
/// what is hidden, and then the body, no deeper than `DEPTH`.
#[derive(Default)]
struct Steps {
    depth: usize,
    body: bool,
    hidden: Hidden,
    /// The automatic style being read.
    style: String,
}

/// The names of the automatic styles that hide what uses them.
#[derive(Default)]
struct Hidden {
    /// Of paragraphs and spans, as Word's hidden text is.
    text: HashSet<String>,
    sheets: HashSet<String>,
    /// Of slides skipped in a show.
    slides: HashSet<String>,
}

impl Steps {
    /// Whether `node` is a step of the body for the reader to take.
    fn pass(&mut self, node: &Node) -> bool {
        match node {
            Node::Open(tag) => {
                self.depth += 1;
                if self.depth > DEPTH {
                    return false;
                }
                if !self.body {
                    self.style_step(tag);
                    self.body = local(tag) == b"body";
                    return false;
                }
                true
            }
            Node::Close(_) => {
                let depth = self.depth;
                self.depth = depth.saturating_sub(1);
                self.body && depth <= DEPTH
            }
            Node::Text(_) => self.body && self.depth <= DEPTH,
        }
    }

    /// Notes a style that hides what uses it.
    fn style_step(&mut self, tag: &BytesStart) {
        let is = |name: &[u8], value: &str| attr(tag, name).as_deref() == Some(value);
        let hidden = &mut self.hidden;
        let into = match local(tag) {
            b"style" => {
                self.style = attr(tag, b"name").unwrap_or_default();
                return;
            }
            b"text-properties" if is(b"display", "none") => &mut hidden.text,
            b"table-properties" if is(b"display", "false") => &mut hidden.sheets,
            b"drawing-page-properties" if is(b"visibility", "hidden") => &mut hidden.slides,
            _ => return,
        };
        into.insert(self.style.clone());
    }
}

/// content.xml, unpacked, from a document not locked with a password.
fn content(package: &mut Package) -> Result<Vec<u8>, String> {
    locked(package)?;
    package
        .part(CONTENT)?
        .ok_or_else(|| format!("it has no {CONTENT}"))
}

/// Refused when the manifest says the document's parts are encrypted, which
/// would otherwise read as broken XML.
fn locked(package: &mut Package) -> Result<(), String> {
    let Some(bytes) = package.part("META-INF/manifest.xml")? else {
        return Ok(());
    };
    let mut locked = false;
    walk(&bytes[..], package.until, |node| {
        locked |= matches!(node, Node::Open(tag) if local(tag) == b"encryption-data");
        !locked
    })?;
    if locked {
        return Err("it is locked with a password; save it again without one and read that".into());
    }
    Ok(())
}

/// Whether `tag` is a picture's title or description, said beside its text.
fn alternative(tag: &BytesStart) -> bool {
    matches!(local(tag), b"title" | b"desc")
        && tag.name().prefix().is_some_and(|p| p.as_ref() == b"svg")
}

/// What `tag` stands for when it is a space, a tab or a line break.
fn spaces(tag: &BytesStart) -> Option<String> {
    Some(match local(tag) {
        // A count in the billions would fill the memory from a few bytes; a
        // line's worth is all a reader can see of it.
        b"s" => {
            let count: usize = attr(tag, b"c")
                .and_then(|c| c.trim().parse().ok())
                .unwrap_or(1);
            " ".repeat(count.clamp(1, crate::tools::LINE_CHARS))
        }
        b"tab" => "\t".into(),
        b"line-break" => "\n".into(),
        _ => return None,
    })
}

/// Adds the XML's own characters to `text`, where a run of spaces, tabs and
/// line ends shows as one space, and none at the start.
fn soft(text: &mut String, spaced: &mut bool, chunk: &str) {
    for c in chunk.chars() {
        if matches!(c, ' ' | '\t' | '\n' | '\r') {
            if !*spaced && !text.is_empty() {
                text.push(' ');
                *spaced = true;
            }
        } else {
            text.push(c);
            *spaced = false;
        }
    }
}

/// Adds spaces an element stands for, which count each.
fn hard(text: &mut String, spaced: &mut bool, chunk: &str) {
    text.push_str(chunk);
    *spaced = false;
}

/// Paragraphs, lists and tables of OpenDocument text as lines, in the order
/// they end.
#[derive(Default)]
struct Flow {
    lines: Vec<String>,
    /// How deep the walk is in an element left out.
    skip: usize,
    paragraphs: Vec<Paragraph>,
    /// For each list open, whether its item open has shown its mark.
    lists: Vec<bool>,
    tables: Vec<Table>,
    /// Bytes of the lines finished, a table's counted again each time they
    /// go into a cell of the table around it.
    bytes: usize,
    /// Bytes of the paragraphs open, which nest in text boxes.
    open: usize,
    /// Whether either passed `VIEW_BYTES`, which stops the walk.
    full: bool,
}

/// A paragraph being read.
#[derive(Default)]
struct Paragraph {
    text: String,
    /// Whether the text ends in a space that the next ones join.
    spaced: bool,
    /// Its heading level, 0 for none.
    heading: usize,
}

impl Flow {
    /// Takes the next step of the walk, leaving out text in a style of
    /// `hidden`; true until the lines are full.
    fn take(&mut self, node: Node, hidden: &HashSet<String>) -> bool {
        if self.skip > 0 {
            match node {
                Node::Open(_) => self.skip += 1,
                Node::Close(_) => self.skip -= 1,
                Node::Text(_) => {}
            }
            return true;
        }
        match node {
            Node::Open(tag) => self.open(tag, hidden),
            Node::Close(name) => self.close(name),
            Node::Text(text) => self.push(text, false),
        }
        !self.full
    }

    fn open(&mut self, tag: &BytesStart, hidden: &HashSet<String>) {
        let name = local(tag);
        let styled = matches!(name, b"p" | b"h" | b"span");
        if LEFT_OUT.contains(&name)
            // The templates an index is made from, not the index.
            || name.ends_with(b"-source")
            || alternative(tag)
            || (styled && attr(tag, b"style-name").is_some_and(|style| hidden.contains(&style)))
        {
            self.skip = 1;
            return;
        }
        if let Some(spaces) = spaces(tag) {
            self.push(&spaces, true);
            return;
        }
        match name {
            b"p" | b"h" => {
                let level = attr(tag, b"outline-level").and_then(|n| n.trim().parse().ok());
                // Markdown goes six levels deep, where OpenDocument goes ten.
                let heading = match name {
                    b"h" => level.unwrap_or(1).clamp(1, 6),
                    _ => 0,
                };
                self.paragraphs.push(Paragraph {
                    heading,
                    ..Paragraph::default()
                });
            }
            b"list" => self.lists.push(false),
            // A header is an item with no mark.
            b"list-item" | b"list-header" => {
                if let Some(marked) = self.lists.last_mut() {
                    *marked = name == b"list-header";
                }
            }
            b"table" => self.tables.push(Table::default()),
            b"table-row" => {
                if let Some(table) = self.tables.last_mut() {
                    table.rows.push(Vec::new());
                }
            }
            b"table-cell" | b"covered-table-cell" => {
                if let Some(table) = self.tables.last_mut() {
                    table.cell = Some(String::new());
                }
            }
            _ => {}
        }
    }

    fn close(&mut self, name: &[u8]) {
        match name {
            b"p" | b"h" => {
                if let Some(paragraph) = self.paragraphs.pop() {
                    self.open -= paragraph.text.len();
                    self.end(paragraph);
                }
            }
            b"list" => {
                self.lists.pop();
            }
            b"table-cell" | b"covered-table-cell" => {
                if let Some(table) = self.tables.last_mut()
                    && let Some(cell) = table.cell.take()
                    && let Some(row) = table.rows.last_mut()
                {
                    row.push(cell);
                }
            }
            b"table" => {
                if let Some(table) = self.tables.pop() {
                    for line in table_lines(table) {
                        self.deliver(line);
                    }
                }
            }
            _ => {}
        }
    }

    /// Adds `text` to the paragraph open: as it is where `hard_spaces`, an
    /// element standing for it, else as the XML's own characters.
    fn push(&mut self, text: &str, hard_spaces: bool) {
        let Some(paragraph) = self.paragraphs.last_mut() else {
            return;
        };
        let before = paragraph.text.len();
        match hard_spaces {
            true => hard(&mut paragraph.text, &mut paragraph.spaced, text),
            false => soft(&mut paragraph.text, &mut paragraph.spaced, text),
        }
        self.open += paragraph.text.len() - before;
        self.full |= self.open > VIEW_BYTES;
    }

    /// Makes a finished paragraph a line, marked as a heading or list item.
    fn end(&mut self, paragraph: Paragraph) {
        let text = paragraph.text.trim();
        if text.is_empty() {
            return;
        }
        let depth = self.lists.len().saturating_sub(1).min(8);
        let line = match (paragraph.heading, self.lists.last_mut()) {
            (0, Some(marked)) if !*marked => {
                *marked = true;
                format!("{}- {text}", "  ".repeat(depth))
            }
            // The item's later paragraphs, under its first.
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
    use super::super::{DEADLINE, Package, View};
    use super::*;
    use std::time::{Duration, Instant};

    const NS: &str = r#"xmlns:office="urn:oasis:names:tc:opendocument:xmlns:office:1.0" xmlns:style="urn:oasis:names:tc:opendocument:xmlns:style:1.0" xmlns:text="urn:oasis:names:tc:opendocument:xmlns:text:1.0" xmlns:table="urn:oasis:names:tc:opendocument:xmlns:table:1.0" xmlns:draw="urn:oasis:names:tc:opendocument:xmlns:drawing:1.0" xmlns:presentation="urn:oasis:names:tc:opendocument:xmlns:presentation:1.0" xmlns:svg="urn:oasis:names:tc:opendocument:xmlns:svg-compatible:1.0" xmlns:dc="http://purl.org/dc/elements/1.1/""#;

    /// An OpenDocument file of `kind`, such as "text", whose content.xml has
    /// `styles` as its automatic styles and `body` as its body.
    pub(crate) fn odf(kind: &str, styles: &str, body: &str) -> Vec<u8> {
        let mime = format!("application/vnd.oasis.opendocument.{kind}");
        let content = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?><office:document-content {NS} office:version="1.3"><office:automatic-styles>{styles}</office:automatic-styles><office:body><office:{kind}>{body}</office:{kind}></office:body></office:document-content>"#
        );
        let manifest = format!(
            r#"<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0"><manifest:file-entry manifest:full-path="/" manifest:media-type="{mime}"/><manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml"/></manifest:manifest>"#
        );
        package(&[
            ("mimetype", &mime),
            ("META-INF/manifest.xml", &manifest),
            ("content.xml", &content),
        ])
    }

    #[test]
    fn an_odt_reads_with_headings_lists_and_tables() {
        let styles = r#"<style:style style:name="T1" style:family="text"><style:text-properties text:display="none"/></style:style>"#;
        let cell = |text: &str| {
            format!(
                "<table:table-cell office:value-type=\"string\"><text:p>{text}</text:p></table:table-cell>"
            )
        };
        let body = [
            r#"<text:sequence-decls><text:sequence-decl text:display-outline-level="0" text:name="Table"/></text:sequence-decls>"#.to_owned(),
            r#"<text:h text:style-name="Heading_20_1" text:outline-level="1">Plan</text:h>"#.into(),
            r#"<text:h text:outline-level="12">Deep</text:h>"#.into(),
            // The XML's own line ends and indents count as one space.
            "<text:p>Rent is\n    due <text:span>monthly</text:span> &amp; on<text:s text:c=\"2\"/>time&#x2E;<text:span text:style-name=\"T1\">hidden</text:span><text:note><text:note-citation>1</text:note-citation><text:note-body><text:p>A footnote</text:p></text:note-body></text:note><office:annotation><dc:creator>Ann</dc:creator><text:p>A comment</text:p></office:annotation></text:p>".into(),
            "<text:tracked-changes><text:changed-region><text:deletion><text:p>Deleted</text:p></text:deletion></text:changed-region></text:tracked-changes>".into(),
            "<text:list><text:list-item><text:p>Deposit</text:p><text:p>paid in May</text:p><text:list><text:list-item><text:p>Receipt</text:p></text:list-item></text:list></text:list-item><text:list-item><text:p>Keys</text:p></text:list-item></text:list>".into(),
            "<text:p>Line one<text:line-break/>line two<text:tab/>tabbed</text:p>".into(),
            format!(
                "<table:table table:name=\"Costs\"><table:table-column table:number-columns-repeated=\"2\"/><table:table-header-rows><table:table-row>{}{}</table:table-row></table:table-header-rows><table:table-row>{}{}</table:table-row></table:table>",
                cell("Item"),
                cell("Cost"),
                cell("Desk | chair"),
                cell("40")
            ),
            r#"<text:p><draw:frame draw:name="Picture"><svg:title>A photo</svg:title><draw:text-box><text:p>Boxed</text:p></draw:text-box></draw:frame></text:p>"#.into(),
            r#"<text:table-of-content><text:table-of-content-source><text:index-title-template>Contents</text:index-title-template></text:table-of-content-source></text:table-of-content>"#.into(),
        ]
        .concat();
        let view = view_of("plan.odt", &odf("text", styles, &body))
            .unwrap()
            .unwrap();
        assert_eq!(
            view,
            "# Plan\n###### Deep\nRent is due monthly & on  time.\n- Deposit\n  paid in May\n  - Receipt\n- Keys\nLine one\nline two\ttabbed\n\
             | Item | Cost |\n| --- | --- |\n| Desk \\| chair | 40 |\nBoxed"
        );
        let path = std::path::Path::new("plan.odt");
        let lines: Vec<&[u8]> = view.lines().map(str::as_bytes).collect();
        assert_eq!(super::super::places(path, &lines), [0, 1]);
    }

    #[test]
    fn an_ods_view_names_each_sheet_and_its_size() {
        let styles = r#"<style:style style:name="ta2" style:family="table"><style:table-properties table:display="false"/></style:style>"#;
        let budget = concat!(
            r#"<table:table table:name="Budget" table:style-name="ta1"><table:table-column table:number-columns-repeated="1024"/>"#,
            r#"<table:table-row><table:table-cell office:value-type="string"><text:p>Month</text:p></table:table-cell><table:table-cell office:value-type="string"><text:p>Amount</text:p></table:table-cell></table:table-row>"#,
            // Values as they show, not as they are saved.
            r#"<table:table-row><table:table-cell office:value-type="date" office:date-value="2024-07-01"><text:p>07/01/24</text:p></table:table-cell><table:table-cell office:value-type="currency" office:value="1250.5"><text:p>$1,250.50</text:p></table:table-cell><table:table-cell office:value-type="float" office:value="12"/><table:table-cell office:value-type="boolean" office:boolean-value="true"/></table:table-row>"#,
            r#"<table:table-row table:number-rows-repeated="2"><table:table-cell table:number-columns-repeated="1024"/></table:table-row>"#,
            r#"<table:table-row><table:table-cell office:value-type="string"><office:annotation><dc:creator>Ann</dc:creator><text:p>Check</text:p></office:annotation><text:p>Rent</text:p><text:p>(flat)</text:p></table:table-cell><table:table-cell table:number-columns-spanned="2" office:value-type="string"><text:p>wide</text:p></table:table-cell><table:covered-table-cell/><table:table-cell table:number-columns-repeated="2" office:value-type="float" office:value="0"><text:p>0</text:p></table:table-cell></table:table-row>"#,
            r#"<table:table-row table:number-rows-repeated="1048570"><table:table-cell table:number-columns-repeated="1024"/></table:table-row></table:table>"#,
        );
        let old = r#"<table:table table:name="Old" table:style-name="ta2"><table:table-row><table:table-cell/><table:table-cell office:value-type="float" office:value="7"><text:p>7</text:p></table:table-cell></table:table-row></table:table>"#;
        let empty = r#"<table:table table:name="Empty"><table:table-row table:number-rows-repeated="1048576"><table:table-cell table:number-columns-repeated="1024"/></table:table-row></table:table>"#;
        let body = [budget, old, empty].concat();
        let view = view_of("budget.ods", &odf("spreadsheet", styles, &body));
        assert_eq!(
            view,
            Ok(Some(
                "## Sheet \"Budget\" (5 rows x 5 cols)\n\
                 Month\tAmount\n\
                 07/01/24\t$1,250.50\t12\tTRUE\n\
                 \n\
                 \n\
                 Rent (flat)\twide\t\t0\t0\n\
                 \n\
                 ## Sheet \"Old\" (hidden, 1 rows x 2 cols)\n\
                 \t7\n\
                 \n\
                 ## Sheet \"Empty\" (empty)"
                    .into()
            ))
        );
    }

    #[test]
    fn rows_and_cells_repeated_past_the_limits_are_counted_not_copied() {
        // One cell repeated across four billion columns, in a row repeated
        // four billion times: a few hundred bytes that would be exabytes.
        let body = r#"<table:table table:name="Data"><table:table-row table:number-rows-repeated="4000000000"><table:table-cell table:number-columns-repeated="4000000000" office:value-type="string"><text:p>x</text:p></table:table-cell></table:table-row></table:table>"#;
        let started = Instant::now();
        let view = view_of("repeat.ods", &odf("spreadsheet", "", body))
            .unwrap()
            .unwrap();
        assert!(started.elapsed() < Duration::from_secs(10));
        let lines: Vec<&str> = view.lines().collect();
        assert_eq!(
            lines[0],
            "## Sheet \"Data\" (4000000000 rows x 4000000000 cols)"
        );
        assert_eq!(lines.len(), 1 + 5_000 + 2);
        assert_eq!(lines[1], vec!["x"; 256].join("\t"));
        assert_eq!(
            &lines[5_001..],
            [
                "[Only the first 5000 rows are shown.]",
                "[Only the first 256 columns, A to IV, are shown.]"
            ]
        );
    }

    #[test]
    fn a_long_text_repeated_through_a_sheet_stops_at_the_view_limit() {
        // Each row 2.5 MB, so six rows fill the view.
        let long = "y".repeat(10_000);
        let body = format!(
            r#"<table:table table:name="Wide"><table:table-row table:number-rows-repeated="5000"><table:table-cell table:number-columns-repeated="256" office:value-type="string"><text:p>{long}</text:p></table:table-cell></table:table-row></table:table>"#
        );
        let view = view_of("wide.ods", &odf("spreadsheet", "", &body))
            .unwrap()
            .unwrap();
        assert!(
            (15_000_000..=VIEW_BYTES + 100).contains(&view.len()),
            "{}",
            view.len()
        );
        assert!(view.starts_with("## Sheet \"Wide\" (at least 5000 rows x 256 cols)\n"));
        assert!(view.ends_with("\n\n[The rest is not shown: the view stops at 16 MB.]"));
    }

    #[test]
    fn a_spreadsheet_cut_at_the_unpack_limit_holds_at_least_what_was_read() {
        let rows: String = (1..=200)
            .map(|n| {
                format!(
                    r#"<table:table-row><table:table-cell office:value-type="float" office:value="{n}"><text:p>{n}</text:p></table:table-cell></table:table-row>"#
                )
            })
            .collect();
        let body = format!(r#"<table:table table:name="Data">{rows}</table:table>"#);
        let dir = std::env::temp_dir().join(format!("letronna-odf-{}-cut", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cut.ods");
        std::fs::write(&path, odf("spreadsheet", "", &body)).unwrap();
        let mut package = Package::open(&path, Instant::now() + DEADLINE).unwrap();
        // Room for the manifest and the start of the sheet.
        package.left = 3_000;
        let mut view = View::default();
        spreadsheet(&mut package, &mut view).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();
        let view = view.finish();
        let lines: Vec<&str> = view.lines().collect();
        let shown = lines.len() - 2;
        assert!((5..200).contains(&shown), "{view}");
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
    fn an_odp_reads_slide_by_slide_with_notes() {
        let styles = r#"<style:style style:name="dp2" style:family="drawing-page"><style:drawing-page-properties presentation:visibility="hidden"/></style:style>"#;
        let frame = |class: &str, text: &str| {
            format!(
                r#"<draw:frame presentation:class="{class}"><draw:text-box><text:p>{text}</text:p></draw:text-box></draw:frame>"#
            )
        };
        let welcome = format!(
            r#"<draw:page draw:name="page1" draw:style-name="dp1">{}{}<presentation:notes><draw:page-thumbnail draw:page-number="1"/>{}</presentation:notes></draw:page>"#,
            frame("title", "Welcome"),
            r#"<draw:frame presentation:class="outline"><draw:text-box><text:list><text:list-item><text:p>Agenda:</text:p></text:list-item><text:list-item><text:p>Budget</text:p></text:list-item></text:list></draw:text-box></draw:frame>"#,
            frame("notes", "Say hello")
        );
        let table = r#"<draw:page draw:name="page2" draw:style-name="dp2"><draw:frame><table:table><table:table-row><table:table-cell><text:p>Q1</text:p></table:table-cell><table:table-cell><text:p>Q2</text:p></table:table-cell></table:table-row></table:table></draw:frame><presentation:notes><draw:page-thumbnail/></presentation:notes></draw:page>"#;
        let blank = r#"<draw:page draw:name="page3"/>"#;
        let body = [welcome.as_str(), table, blank].concat();
        let view = view_of("talk.odp", &odf("presentation", styles, &body));
        assert_eq!(
            view,
            Ok(Some(
                "## Slide 1\nWelcome\n- Agenda:\n- Budget\n### Notes\nSay hello\n\n\
                 ## Slide 2 (hidden)\n| Q1 | Q2 |\n| --- | --- |\n\n\
                 ## Slide 3"
                    .into()
            ))
        );
    }

    #[test]
    fn templates_read_as_their_kind() {
        let letter = odf("text", "", "<text:p>Dear customer</text:p>");
        assert_eq!(
            view_of("letter.ott", &letter),
            Ok(Some("Dear customer".into()))
        );
        assert!(super::super::is_extension("OTS") && super::super::is_extension("otp"));
    }

    #[test]
    fn a_document_missing_its_content_or_broken_says_why() {
        let none = package(&[("mimetype", "application/vnd.oasis.opendocument.text")]);
        for name in ["none.odt", "none.ods", "none.odp"] {
            let extension = name.rsplit('.').next().unwrap();
            assert_eq!(
                view_of(name, &none),
                Err(format!(
                    "Could not read this .{extension}: it has no content.xml."
                ))
            );
        }
        let broken = package(&[(
            "content.xml",
            "<office:document-content><office:body><text:p>a</text:q>",
        )]);
        let why = view_of("broken.odt", &broken).unwrap_err();
        assert!(
            why.starts_with("Could not read this .odt: its XML is broken ("),
            "{why}"
        );
        let why = view_of("broken.ods", &broken).unwrap_err();
        assert!(
            why.starts_with("Could not read this .ods: its XML is broken ("),
            "{why}"
        );
        // Cut short, or no zip at all.
        let whole = odf("text", "", "<text:p>Cut</text:p>");
        let cut = view_of("cut.odt", &whole[..whole.len() / 2]).unwrap_err();
        assert!(
            cut.starts_with("Could not read this .odt: it is damaged or cut short ("),
            "{cut}"
        );
        assert_eq!(view_of("notes.odt", b"plain words"), Ok(None));
        // A body with nothing in it.
        assert_eq!(
            view_of("blank.odp", &odf("presentation", "", "")),
            Ok(Some("[This document holds no text.]".into()))
        );
    }

    #[test]
    fn a_document_locked_with_a_password_says_so() {
        let manifest = r#"<manifest:manifest xmlns:manifest="urn:oasis:names:tc:opendocument:xmlns:manifest:1.0"><manifest:file-entry manifest:full-path="content.xml" manifest:media-type="text/xml" manifest:size="2048"><manifest:encryption-data manifest:checksum-type="SHA1/1K" manifest:checksum="x"><manifest:algorithm manifest:algorithm-name="Blowfish CFB" manifest:initialisation-vector="y"/></manifest:encryption-data></manifest:file-entry></manifest:manifest>"#;
        let locked = package(&[
            ("mimetype", "application/vnd.oasis.opendocument.spreadsheet"),
            ("META-INF/manifest.xml", manifest),
            ("content.xml", "\u{1}\u{2}scrambled"),
        ]);
        assert_eq!(
            view_of("pay.ods", &locked),
            Err("Could not read this .ods: it is locked with a password; save it again without one and read that.".into())
        );
    }

    #[test]
    fn a_zip_bomb_is_refused_before_inflating() {
        let mut bomb = odf("text", "", "<text:p>boom</text:p>");
        say_unpacked(&mut bomb, "content.xml", 3 << 30);
        assert_eq!(
            view_of("bomb.odt", &bomb),
            Err("Could not read this .odt: it unpacks to more than 50 MB.".into())
        );
        let mut small = odf("presentation", "", "<draw:page/>");
        say_unpacked(&mut small, "content.xml", 5);
        assert_eq!(
            view_of("small.odp", &small),
            Err("Could not read this .odp: its content.xml unpacks to more than it says.".into())
        );
        // A spreadsheet is walked as it unpacks, held to what is left to
        // unpack rather than to what its zip says, as a workbook's sheet is.
        let sheet = odf(
            "spreadsheet",
            "",
            r#"<table:table table:name="S"><table:table-row><table:table-cell><text:p>1</text:p></table:table-cell></table:table-row></table:table>"#,
        );
        for says in [5, 3 << 30] {
            let mut told = sheet.clone();
            say_unpacked(&mut told, "content.xml", says);
            assert_eq!(
                view_of("told.ods", &told),
                Ok(Some("## Sheet \"S\" (1 rows x 1 cols)\n1".into())),
                "{says}"
            );
        }
    }

    #[test]
    fn elements_nested_thousands_deep_are_read_no_deeper_than_the_limit() {
        // 200,000 open spans and tables: each would hold memory while open.
        let depth = 200_000;
        let spans = format!(
            "<text:p>top{}deep{}</text:p>",
            "<text:span>".repeat(depth),
            "</text:span>".repeat(depth)
        );
        let tables = format!(
            "{}<text:p>deep</text:p>{}",
            "<table:table><table:table-row><table:table-cell>".repeat(depth / 3),
            "</table:table-cell></table:table-row></table:table>".repeat(depth / 3)
        );
        let lists = format!(
            "{}<text:p>deep</text:p>{}",
            "<text:list><text:list-item>".repeat(depth / 2),
            "</text:list-item></text:list>".repeat(depth / 2)
        );
        for body in [spans, tables, lists] {
            let started = Instant::now();
            let view = view_of("nested.odt", &odf("text", "", &body))
                .unwrap()
                .unwrap();
            assert!(started.elapsed() < Duration::from_secs(10));
            assert!(!view.contains("deep"), "{}", &view[..view.len().min(300)]);
        }
        let sheet = format!(
            r#"<table:table table:name="S"><table:table-row><table:table-cell><text:p>{}x{}</text:p></table:table-cell></table:table-row></table:table>"#,
            "<text:span>".repeat(depth),
            "</text:span>".repeat(depth)
        );
        let view = view_of("nested.ods", &odf("spreadsheet", "", &sheet));
        assert_eq!(view, Ok(Some("## Sheet \"S\" (empty)".into())));
    }

    #[test]
    fn a_long_text_stops_at_the_view_limit() {
        // 30 MB of text in 40 KB paragraphs, within what a document may
        // unpack to.
        let body = format!("<text:p>{}</text:p>", "z".repeat(40_000)).repeat(750);
        let view = view_of("long.odt", &odf("text", "", &body))
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
    fn a_space_count_in_the_billions_stands_for_a_line_at_most() {
        let body = r#"<text:p>a<text:s text:c="4000000000"/>b<text:s text:c="-1"/>c</text:p>"#;
        let view = view_of("spaces.odt", &odf("text", "", body))
            .unwrap()
            .unwrap();
        // "a", a line's worth of spaces and "b c", split there as any line
        // longer than read_file shows is, without the spaces it was split at.
        assert_eq!(view, "a\n b c");
    }

    #[test]
    fn entities_a_document_declares_are_not_expanded() {
        let laughs = format!(
            r#"<!DOCTYPE d [<!ENTITY a "{}"><!ENTITY b "&a;&a;&a;&a;&a;&a;&a;&a;">]><office:document-content {NS}><office:body><office:text><text:p>Hi &b;&b;</text:p></office:text></office:body></office:document-content>"#,
            "ha".repeat(1_000)
        );
        let file = package(&[("content.xml", &laughs)]);
        assert_eq!(view_of("laughs.odt", &file), Ok(Some("Hi".into())));
    }
}
