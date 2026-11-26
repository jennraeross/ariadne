//! Manuscript import for the formats writers actually receive.
//!
//! A sheet holds Markdown, so a Word, OpenDocument, RTF, HTML or EPUB file
//! is read into the craft-libs word model (`craft-wordmodel`) and written
//! out as Markdown. No layout or theme mapping is involved: the document
//! structure is all a sheet keeps.
//!
//! Pictures are not imported yet: `craft_markdown` writes each one as
//! `![alt](image.ext)` with no bytes, so they become a visible
//! `*[Image: alt]*` placeholder and are counted for the status line.
//! Equations are dropped by the Markdown writer; see
//! `docs/craft-libs-integration.md` for the follow-ups.

use std::collections::HashMap;
use std::path::Path;

use craft_wordmodel::Document;

/// The manuscript formats a drop or the Import command can read.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Format {
    Docx,
    Doc,
    Odt,
    Rtf,
    Html,
    Epub,
    Markdown,
    Text,
}

impl Format {
    /// Menu and status text.
    pub fn label(self) -> &'static str {
        match self {
            Format::Docx => "Word document",
            Format::Doc => "Word 97–2003 document",
            Format::Odt => "OpenDocument text",
            Format::Rtf => "Rich Text",
            Format::Html => "HTML",
            Format::Epub => "EPUB",
            Format::Markdown => "Markdown",
            Format::Text => "plain text",
        }
    }

    /// Extensions the native file dialog offers.
    pub fn extensions(self) -> &'static [&'static str] {
        match self {
            Format::Docx => &["docx"],
            Format::Doc => &["doc"],
            Format::Odt => &["odt"],
            Format::Rtf => &["rtf"],
            Format::Html => &["html", "htm"],
            Format::Epub => &["epub"],
            Format::Markdown => &["md", "markdown"],
            Format::Text => &["txt"],
        }
    }

    /// True when the file is already what a sheet holds.
    fn is_plain(self) -> bool {
        matches!(self, Format::Markdown | Format::Text)
    }
}

/// One imported document, ready to become a sheet.
#[derive(Debug)]
pub struct ImportedSheet {
    /// A chapter title for EPUB, empty when the caller names the sheet.
    pub title: String,
    pub markdown: String,
}

/// Everything one manuscript produced: an EPUB is a sheet per chapter, the
/// other formats are one sheet.
#[derive(Debug)]
pub struct Imported {
    pub sheets: Vec<ImportedSheet>,
    /// Pictures the reader found but the Markdown writer cannot carry.
    pub pictures_dropped: usize,
}

/// The format of a file by name, falling back to its first bytes so a
/// misnamed manuscript still opens.
pub fn sniff(name: &str, bytes: &[u8]) -> Option<Format> {
    from_extension(name).or_else(|| from_bytes(bytes))
}

fn from_extension(name: &str) -> Option<Format> {
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())?
        .to_ascii_lowercase();
    Some(match ext.as_str() {
        "docx" => Format::Docx,
        "doc" => Format::Doc,
        "odt" => Format::Odt,
        "rtf" => Format::Rtf,
        "html" | "htm" => Format::Html,
        "epub" => Format::Epub,
        "md" | "markdown" => Format::Markdown,
        "txt" | "text" => Format::Text,
        _ => return None,
    })
}

/// What the bytes say they are, for a name without a known extension.
/// Package formats are told apart by entry names, which ZIP stores as plain
/// bytes in the local headers; the `mimetype` entry of ODF and EPUB is
/// required to be stored uncompressed, so its text is always visible here.
fn from_bytes(bytes: &[u8]) -> Option<Format> {
    let head = &bytes[..bytes.len().min(4096)];
    if head.starts_with(b"{\\rtf") {
        return Some(Format::Rtf);
    }
    if head.starts_with(b"PK\x03\x04") {
        let probe = &bytes[..bytes.len().min(64 * 1024)];
        if contains(probe, b"application/epub+zip") {
            return Some(Format::Epub);
        }
        if contains(probe, b"application/vnd.oasis.opendocument.text") {
            return Some(Format::Odt);
        }
        if contains(probe, b"word/document.xml") {
            return Some(Format::Docx);
        }
        return None;
    }
    let start = head
        .iter()
        .position(|b| !b.is_ascii_whitespace())
        .unwrap_or(head.len());
    let text = &head[start..];
    let lower = |s: &[u8]| {
        s.iter()
            .map(|b| b.to_ascii_lowercase())
            .collect::<Vec<u8>>()
    };
    let text = lower(text);
    if text.starts_with(b"<!doctype html") || text.starts_with(b"<html") {
        return Some(Format::Html);
    }
    None
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Read one manuscript. Every reader is bounded and lenient by design;
/// a file none of them understands comes back as an error string.
pub fn read(format: Format, bytes: &[u8]) -> Result<Imported, String> {
    if format.is_plain() {
        return Ok(one_sheet(String::from_utf8_lossy(bytes).into_owned(), 0));
    }
    if format == Format::Epub {
        return epub_sheets(bytes);
    }
    let (markdown, pictures_dropped) = match format {
        Format::Docx => {
            let doc = craft_docx::read(bytes).map_err(|e| e.to_string())?;
            document_markdown(&doc)
        }
        Format::Doc => {
            let doc = craft_doc_binary::read(bytes).map_err(|e| e.to_string())?;
            document_markdown(&doc)
        }
        Format::Odt => {
            let doc = craft_odf::text::import(bytes)?;
            document_markdown(&doc)
        }
        Format::Rtf => {
            let doc = craft_rtf::doc::import(bytes)?;
            document_markdown(&doc)
        }
        Format::Html => {
            let html = String::from_utf8_lossy(bytes);
            document_markdown(&craft_html::doc::import(&html))
        }
        Format::Epub | Format::Markdown | Format::Text => unreachable!("handled above"),
    };
    Ok(one_sheet(markdown, pictures_dropped))
}

fn one_sheet(markdown: String, pictures_dropped: usize) -> Imported {
    Imported {
        sheets: vec![ImportedSheet {
            title: String::new(),
            markdown,
        }],
        pictures_dropped,
    }
}

fn document_markdown(doc: &Document) -> (String, usize) {
    let (markdown, pictures_dropped) = strip_picture_placeholders(&craft_markdown::export(doc));
    (reflow_hard_breaks(&markdown), pictures_dropped)
}

/// A line break in a document is layout, not prose: Word, ODF and EPUB
/// files break lines where the page happened to end, and a sheet wraps its
/// own text. A hard break inside a paragraph becomes a space, one at the
/// end of a paragraph disappears, one alone on a line vanishes, and blank
/// lines still separate paragraphs. Poetry loses its line breaks, which is
/// the price of not littering a novel with backslashes; the original file
/// remains the record of the author's lineation.
fn reflow_hard_breaks(input: &str) -> String {
    let mut out: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in input.lines() {
        let Some(body) = line.strip_suffix('\\') else {
            if current.is_empty() {
                out.push(line.to_string());
                continue;
            }
            if body_is_blank(line) {
                out.push(std::mem::take(&mut current));
                out.push(String::new());
            } else {
                current.push(' ');
                current.push_str(line.trim_start());
                out.push(std::mem::take(&mut current));
            }
            continue;
        };
        let body = body.trim_end();
        if !body.is_empty() {
            if !current.is_empty() {
                current.push(' ');
            }
            current.push_str(body.trim_start());
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out.join("\n")
}

fn body_is_blank(line: &str) -> bool {
    line.trim().is_empty()
}

/// One sheet per chapter: the spine in order. Navigation documents, a
/// cover page and a table of contents are the package's own furniture, not
/// chapters, so they are left out. Each chapter keeps its headings; one
/// that starts without a heading takes its reading-system label, or its
/// position, as an `h1`.
fn epub_sheets(bytes: &[u8]) -> Result<Imported, String> {
    let mut book = craft_epub::Epub::open(bytes).map_err(|e| e.to_string())?;
    let labels: HashMap<String, String> = book
        .toc()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|entry| entry.path.map(|path| (path, entry.label)))
        .collect();
    // Manifest roles say what a file is even when its name does not.
    let structural: HashMap<String, bool> = book
        .package
        .manifest
        .iter()
        .map(|item| {
            let furniture = item
                .properties
                .split_whitespace()
                .any(|property| matches!(property, "nav" | "cover-image"));
            (item.path.clone(), furniture)
        })
        .collect();
    let paths: Vec<String> = book.package.spine_paths().map(str::to_string).collect();
    let mut sheets: Vec<ImportedSheet> = Vec::new();
    let mut pictures_dropped = 0;
    for path in paths {
        if is_structural_part(&path) || structural.get(&path).copied().unwrap_or(false) {
            continue;
        }
        let Some(data) = book.read(&path).map_err(|e| e.to_string())? else {
            continue;
        };
        let html = String::from_utf8_lossy(&data);
        let (markdown, dropped) = document_markdown(&craft_html::doc::import(html.as_ref()));
        pictures_dropped += dropped;
        if markdown.trim().is_empty() {
            continue;
        }
        let label = labels
            .get(&path)
            .map(|label| label.trim().to_string())
            .filter(|label| !label.is_empty());
        // Many EPUBs carry the whole book in one XHTML: split it at its
        // own chapter headings, or it would arrive as one enormous sheet.
        for mut sheet in split_at_headings(markdown) {
            if sheet.title.is_empty() {
                sheet.title = label
                    .clone()
                    .unwrap_or_else(|| format!("Chapter {}", sheets.len() + 1));
            }
            if first_heading(&sheet.markdown).is_none() {
                sheet.markdown = format!("# {}\n\n{}", sheet.title, sheet.markdown);
            }
            sheets.push(sheet);
        }
    }
    if sheets.is_empty() {
        return Err("the EPUB has no readable chapters".to_string());
    }
    Ok(Imported {
        sheets,
        pictures_dropped,
    })
}

/// Split one spine document into chapters.
///
/// A document with its own `h1` chapters splits there. A document with no
/// `h1` at all is a whole book in one file (the IDPF sample is), so its
/// `h2` or `h3` entries are the chapters — an anthology numbers its
/// stories in `h3`. One `h1` followed by many `h2`s is a single chapter
/// with subsections, and stays whole.
fn split_at_headings(markdown: String) -> Vec<ImportedSheet> {
    let h1 = heading_count(&markdown, 1);
    let h2 = heading_count(&markdown, 2);
    let h3 = heading_count(&markdown, 3);
    let level = if h1 >= 2 {
        1
    } else if h1 == 0 && h2 >= 2 {
        2
    } else if h1 == 0 && h3 >= 2 {
        3
    } else {
        0
    };
    if level == 0 {
        let title = first_heading(&markdown).unwrap_or_default();
        return vec![ImportedSheet { title, markdown }];
    }
    let prefix = format!("{} ", "#".repeat(level as usize));
    let mut parts: Vec<(String, String)> = Vec::new();
    let mut current: Option<(String, String)> = None;
    let mut preamble = String::new();
    for line in markdown.lines() {
        if let Some(title) = line.strip_prefix(&prefix) {
            if let Some(done) = current.take() {
                parts.push(done);
            }
            current = Some((title.trim().to_string(), line.to_string()));
        } else if let Some((_, body)) = current.as_mut() {
            body.push('\n');
            body.push_str(line);
        } else {
            preamble.push_str(line);
            preamble.push('\n');
        }
    }
    if let Some(done) = current.take() {
        parts.push(done);
    }
    if parts.is_empty() {
        let title = first_heading(&markdown).unwrap_or_default();
        return vec![ImportedSheet { title, markdown }];
    }
    if !preamble.trim().is_empty() {
        parts[0].1 = format!("{}\n\n{}", preamble.trim_end(), parts[0].1);
    }
    parts
        .into_iter()
        .map(|(title, markdown)| ImportedSheet { title, markdown })
        .collect()
}

fn heading_count(markdown: &str, level: u8) -> usize {
    let prefix = format!("{} ", "#".repeat(level as usize));
    markdown
        .lines()
        .filter(|line| line.starts_with(&prefix))
        .count()
}

/// The package's own pages, not chapters.
fn is_structural_part(path: &str) -> bool {
    let name = path.rsplit('/').next().unwrap_or(path).to_ascii_lowercase();
    let stem = name.split('.').next().unwrap_or("");
    matches!(stem, "nav" | "toc" | "cover")
}

fn first_heading(markdown: &str) -> Option<String> {
    markdown.lines().find_map(|line| {
        line.strip_prefix("# ")
            .map(|title| title.trim().to_string())
            .filter(|title| !title.is_empty())
    })
}

/// `craft_markdown` writes a picture as `![alt](image.ext)` but has no
/// bytes to put behind it. A sheet must not gain a broken reference, so
/// each becomes a visible placeholder and is counted.
fn strip_picture_placeholders(input: &str) -> (String, usize) {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    let mut dropped = 0usize;
    while let Some(start) = rest.find("![") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let Some(alt_end) = after.find(']') else {
            out.push_str(&rest[start..]);
            return (out, dropped);
        };
        let alt = &after[..alt_end];
        let tail = &after[alt_end + 1..];
        let mut consumed = None;
        if let Some(paren) = tail.strip_prefix('(') {
            if let Some(close) = paren.find(')') {
                let target = &paren[..close];
                if is_generated_picture(target) {
                    consumed = Some(&paren[close + 1..]);
                }
            }
        }
        match consumed {
            Some(left) => {
                let alt = alt.trim();
                if alt.is_empty() {
                    out.push_str("*[Image]*");
                } else {
                    out.push_str(&format!("*[Image: {alt}]*"));
                }
                dropped += 1;
                rest = left;
            }
            None => {
                out.push_str(&rest[start..start + 2]);
                rest = after;
            }
        }
    }
    out.push_str(rest);
    (out, dropped)
}

/// The names `craft_markdown` invents for document pictures, plus the empty
/// target it writes for figures and charts.
fn is_generated_picture(target: &str) -> bool {
    if target.is_empty() {
        return true;
    }
    let Some(ext) = target.strip_prefix("image.") else {
        return false;
    };
    matches!(
        ext.to_ascii_lowercase().as_str(),
        "png" | "jpg" | "jpeg" | "gif" | "bmp" | "webp" | "tif" | "tiff" | "svg" | "emf" | "wmf"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docx_roundtrips_text() {
        let doc = Document::from_text("Hello manuscript\n\nSecond paragraph");
        let bytes = craft_docx::write(&doc).expect("write docx");
        let imported = read(Format::Docx, &bytes).expect("read docx");
        assert!(imported.sheets[0].markdown.contains("Hello manuscript"));
        assert!(imported.sheets[0].markdown.contains("Second paragraph"));
        assert_eq!(imported.pictures_dropped, 0);
    }

    #[test]
    fn docx_is_recognised_by_bytes_not_only_extension() {
        let doc = Document::from_text("Misnamed manuscript");
        let bytes = craft_docx::write(&doc).expect("write docx");
        assert_eq!(sniff("notes.bin", &bytes), Some(Format::Docx));
        let imported = read(Format::Docx, &bytes).expect("read docx");
        assert!(imported.sheets[0].markdown.contains("Misnamed manuscript"));
    }

    #[test]
    fn html_keeps_headings_emphasis_and_lists() {
        let html = b"<!doctype html><html><body><h1>Title</h1>\
            <p>Some <strong>bold</strong> and <em>italic</em> text.</p>\
            <ul><li>one</li><li>two</li></ul></body></html>";
        let imported = read(Format::Html, html).expect("read html");
        assert!(imported.sheets[0].markdown.contains("# Title"));
        assert!(imported.sheets[0].markdown.contains("**bold**"));
        assert!(imported.sheets[0].markdown.contains("*italic*"));
        assert!(imported.sheets[0].markdown.contains("- one"));
        assert!(imported.sheets[0].markdown.contains("- two"));
    }

    #[test]
    fn rtf_roundtrips_text() {
        let doc = Document::from_text("Rtf manuscript");
        let rtf = craft_rtf::doc::export(&doc);
        let imported = read(Format::Rtf, rtf.as_bytes()).expect("read rtf");
        assert!(imported.sheets[0].markdown.contains("Rtf manuscript"));
    }

    #[test]
    fn odt_roundtrips_text() {
        let doc = Document::from_text("Odt manuscript");
        let bytes = craft_odf::text::export(&doc).expect("write odt");
        let imported = read(Format::Odt, &bytes).expect("read odt");
        assert!(imported.sheets[0].markdown.contains("Odt manuscript"));
    }

    #[test]
    fn epub_imports_its_spine() {
        let xhtml = craft_epub::xhtml(
            "en",
            "A Book",
            "",
            "",
            "<h1 id=\"c1\">One</h1>\n<p>Chapter text.</p>\n",
        );
        let book = craft_epub::Book {
            metadata: craft_epub::Metadata {
                identifier: craft_epub::urn_uuid("A Book\n1"),
                title: "A Book".into(),
                language: "en".into(),
                modified: "2026-11-26T00:00:00Z".into(),
                ..Default::default()
            },
            resources: vec![craft_epub::Resource::new(
                "content",
                "content.xhtml",
                "application/xhtml+xml",
                xhtml.into_bytes(),
            )],
            spine: vec!["content".into()],
            ..Default::default()
        };
        let bytes = craft_epub::write(&book).expect("write epub");
        let imported = read(Format::Epub, &bytes).expect("read epub");
        assert_eq!(imported.sheets.len(), 1);
        assert_eq!(imported.sheets[0].title, "One");
        assert!(imported.sheets[0].markdown.contains("# One"));
        assert!(imported.sheets[0].markdown.contains("Chapter text."));
    }

    #[test]
    fn epub_is_one_sheet_per_chapter() {
        let one = craft_epub::xhtml("en", "A Book", "", "", "<h1>One</h1>\n<p>First.</p>\n");
        let two = craft_epub::xhtml("en", "A Book", "", "", "<p>Second, no heading.</p>\n");
        let book = craft_epub::Book {
            metadata: craft_epub::Metadata {
                identifier: craft_epub::urn_uuid("A Book\n2"),
                title: "A Book".into(),
                language: "en".into(),
                modified: "2026-11-26T00:00:00Z".into(),
                ..Default::default()
            },
            resources: vec![
                craft_epub::Resource::new(
                    "c1",
                    "ch1.xhtml",
                    "application/xhtml+xml",
                    one.into_bytes(),
                ),
                craft_epub::Resource::new(
                    "c2",
                    "ch2.xhtml",
                    "application/xhtml+xml",
                    two.into_bytes(),
                ),
            ],
            spine: vec!["c1".into(), "c2".into()],
            nav: craft_epub::Nav {
                items: vec![
                    craft_epub::NavItem {
                        href: "ch1.xhtml".into(),
                        label: "Chapter One".into(),
                        level: 1,
                    },
                    craft_epub::NavItem {
                        href: "ch2.xhtml".into(),
                        label: "Chapter Two".into(),
                        level: 1,
                    },
                ],
                ..Default::default()
            },
            ..Default::default()
        };
        let bytes = craft_epub::write(&book).expect("write epub");
        let imported = read(Format::Epub, &bytes).expect("read epub");
        assert_eq!(imported.sheets.len(), 2);
        // The chapter's own heading wins; a chapter without one takes the
        // reading system's label.
        assert_eq!(imported.sheets[0].title, "One");
        assert!(imported.sheets[0].markdown.starts_with("# One"));
        assert_eq!(imported.sheets[1].title, "Chapter Two");
        assert!(imported.sheets[1].markdown.starts_with("# Chapter Two"));
        assert!(imported.sheets[1].markdown.contains("Second, no heading."));
    }

    /// One EPUB whose whole content is the given XHTML files, in spine order.
    fn epub_of(chapters: &[(&str, &str)]) -> Vec<u8> {
        let resources = chapters
            .iter()
            .map(|(path, body)| {
                craft_epub::Resource::new(
                    *path,
                    *path,
                    "application/xhtml+xml",
                    craft_epub::xhtml("en", "Book", "", "", body).into_bytes(),
                )
            })
            .collect();
        let book = craft_epub::Book {
            metadata: craft_epub::Metadata {
                identifier: craft_epub::urn_uuid("Book\n3"),
                title: "Book".into(),
                language: "en".into(),
                modified: "2026-11-26T00:00:00Z".into(),
                ..Default::default()
            },
            resources,
            spine: chapters.iter().map(|(path, _)| path.to_string()).collect(),
            ..Default::default()
        };
        craft_epub::write(&book).expect("write epub")
    }

    #[test]
    fn a_whole_book_in_one_document_splits_at_its_chapters() {
        let bytes = epub_of(&[(
            "all.xhtml",
            "<h1>One</h1><p>First.</p><h1>Two</h1><p>Second.</p>",
        )]);
        let imported = read(Format::Epub, &bytes).expect("read epub");
        assert_eq!(imported.sheets.len(), 2);
        assert_eq!(imported.sheets[0].title, "One");
        assert!(imported.sheets[0].markdown.starts_with("# One"));
        assert_eq!(imported.sheets[1].title, "Two");
        assert!(imported.sheets[1].markdown.starts_with("# Two"));
    }

    #[test]
    fn a_chapter_with_subsections_stays_one_sheet() {
        let bytes = epub_of(&[(
            "ch1.xhtml",
            "<h1>The Long Chapter</h1><h2>First part</h2><p>A.</p>\
             <h2>Second part</h2><p>B.</p>",
        )]);
        let imported = read(Format::Epub, &bytes).expect("read epub");
        assert_eq!(imported.sheets.len(), 1);
        assert_eq!(imported.sheets[0].title, "The Long Chapter");
    }

    #[test]
    fn an_anthology_splits_at_its_story_headings() {
        let bytes = epub_of(&[(
            "all.xhtml",
            "<h2>A Section</h2><h3>190</h3><p>One.</p>\
             <h3>191</h3><p>Two.</p>",
        )]);
        let imported = read(Format::Epub, &bytes).expect("read epub");
        assert_eq!(imported.sheets.len(), 2);
        assert_eq!(imported.sheets[0].title, "190");
        assert_eq!(imported.sheets[1].title, "191");
    }

    #[test]
    fn structural_pages_are_not_chapters() {
        assert!(is_structural_part("OEBPS/nav.xhtml"));
        assert!(is_structural_part("EPUB/toc.xhtml"));
        assert!(is_structural_part("cover.xhtml"));
        assert!(!is_structural_part("OEBPS/chapter1.xhtml"));
        assert!(!is_structural_part("OEBPS/text/part-1.xhtml"));
    }

    #[test]
    fn picture_placeholders_are_not_left_as_broken_links() {
        let (markdown, dropped) =
            strip_picture_placeholders("Before ![A sunset](image.png) after\n\n![chart]()");
        assert_eq!(dropped, 2);
        assert_eq!(
            markdown,
            "Before *[Image: A sunset]* after\n\n*[Image: chart]*"
        );
        assert!(!markdown.contains("!["));
    }

    #[test]
    fn hard_breaks_are_reflowed_into_prose() {
        // A break at the end of a paragraph plus a lone break marker, as a
        // Word- or PDF-converted EPUB produces: one paragraph, then the next.
        assert_eq!(
            reflow_hard_breaks("one arm.\\\n\\\n\nI stare down"),
            "one arm.\n\nI stare down"
        );
        // Breaks inside a paragraph join into one flowing line.
        assert_eq!(
            reflow_hard_breaks("a line that wrapped\\\nanother line"),
            "a line that wrapped another line"
        );
        assert_eq!(reflow_hard_breaks("a\\\nb\\\nc"), "a b c");
        // Text without breaks is untouched, indentation included.
        assert_eq!(
            reflow_hard_breaks("keep\n  this\n\nnormal"),
            "keep\n  this\n\nnormal"
        );
        assert_eq!(reflow_hard_breaks("trailing\\"), "trailing");
    }

    #[test]
    fn ordinary_links_and_images_survive() {
        let input = "A [link](https://example.com) and ![alt](photo.png).";
        let (markdown, dropped) = strip_picture_placeholders(input);
        assert_eq!(dropped, 0);
        assert_eq!(markdown, input);
    }

    #[test]
    fn plain_text_passes_through_untouched() {
        let imported = read(Format::Text, b"# Not Markdown? It is.").expect("text");
        assert_eq!(imported.sheets[0].markdown, "# Not Markdown? It is.");
        assert_eq!(imported.pictures_dropped, 0);
    }

    #[test]
    fn unknown_names_fall_through() {
        assert_eq!(sniff("photo.png", b"\x89PNG\r\n\x1a\n"), None);
        assert_eq!(sniff("archive.zip", b"PK\x03\x04"), None);
    }

    #[test]
    fn broken_manuscript_is_an_error_not_a_panic() {
        let err = read(Format::Docx, b"not a zip").unwrap_err();
        assert!(!err.is_empty());
    }

    /// Manual check against a file another program wrote (Word, LibreOffice,
    /// Pages). Set `ARIADNE_IMPORT_FIXTURE` to its path and run
    /// `cargo test import::tests::external_fixture -- --ignored --nocapture`.
    #[test]
    #[ignore]
    #[cfg(not(target_arch = "wasm32"))]
    fn external_fixture() {
        let Ok(path) = std::env::var("ARIADNE_IMPORT_FIXTURE") else {
            return;
        };
        let bytes = std::fs::read(&path).expect("read fixture");
        let format = sniff(&path, &bytes).expect("recognised format");
        let imported = read(format, &bytes).expect("import fixture");
        println!(
            "{}: {} sheets, {} pictures dropped",
            format.label(),
            imported.sheets.len(),
            imported.pictures_dropped
        );
        for (n, sheet) in imported.sheets.iter().enumerate() {
            println!(
                "--- sheet {} ({}) ---\n{}",
                n + 1,
                sheet.title,
                sheet.markdown
            );
        }
    }
}
