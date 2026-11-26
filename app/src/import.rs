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

/// One imported manuscript, ready to become a sheet.
#[derive(Debug)]
pub struct Imported {
    pub markdown: String,
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
pub fn to_markdown(format: Format, bytes: &[u8]) -> Result<Imported, String> {
    if format.is_plain() {
        return Ok(Imported {
            markdown: String::from_utf8_lossy(bytes).into_owned(),
            pictures_dropped: 0,
        });
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
        Format::Epub => epub_markdown(bytes)?,
        Format::Markdown | Format::Text => unreachable!("handled above"),
    };
    Ok(Imported {
        markdown,
        pictures_dropped,
    })
}

fn document_markdown(doc: &Document) -> (String, usize) {
    strip_picture_placeholders(&craft_markdown::export(doc))
}

/// One sheet per manuscript: the spine in order, each chapter keeping its
/// own headings. A chapter the reading system names but whose XHTML starts
/// without a heading gets the reading-system label as its `h1`.
fn epub_markdown(bytes: &[u8]) -> Result<(String, usize), String> {
    let mut book = craft_epub::Epub::open(bytes).map_err(|e| e.to_string())?;
    let labels: HashMap<String, String> = book
        .toc()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|entry| entry.path.map(|path| (path, entry.label)))
        .collect();
    let paths: Vec<String> = book.package.spine_paths().map(str::to_string).collect();
    let mut chapters: Vec<String> = Vec::new();
    let mut pictures_dropped = 0;
    for path in paths {
        let Some(data) = book.read(&path).map_err(|e| e.to_string())? else {
            continue;
        };
        let html = String::from_utf8_lossy(&data);
        let (mut markdown, dropped) = document_markdown(&craft_html::doc::import(html.as_ref()));
        pictures_dropped += dropped;
        if markdown.trim().is_empty() {
            continue;
        }
        if let Some(label) = labels.get(&path) {
            if !markdown.trim_start().starts_with("# ") {
                markdown = format!("# {}\n\n{markdown}", label.trim());
            }
        }
        chapters.push(markdown);
    }
    if chapters.is_empty() {
        return Err("the EPUB has no readable chapters".to_string());
    }
    Ok((chapters.join("\n\n"), pictures_dropped))
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
        let imported = to_markdown(Format::Docx, &bytes).expect("read docx");
        assert!(imported.markdown.contains("Hello manuscript"));
        assert!(imported.markdown.contains("Second paragraph"));
        assert_eq!(imported.pictures_dropped, 0);
    }

    #[test]
    fn docx_is_recognised_by_bytes_not_only_extension() {
        let doc = Document::from_text("Misnamed manuscript");
        let bytes = craft_docx::write(&doc).expect("write docx");
        assert_eq!(sniff("notes.bin", &bytes), Some(Format::Docx));
        let imported = to_markdown(Format::Docx, &bytes).expect("read docx");
        assert!(imported.markdown.contains("Misnamed manuscript"));
    }

    #[test]
    fn html_keeps_headings_emphasis_and_lists() {
        let html = b"<!doctype html><html><body><h1>Title</h1>\
            <p>Some <strong>bold</strong> and <em>italic</em> text.</p>\
            <ul><li>one</li><li>two</li></ul></body></html>";
        let imported = to_markdown(Format::Html, html).expect("read html");
        assert!(imported.markdown.contains("# Title"));
        assert!(imported.markdown.contains("**bold**"));
        assert!(imported.markdown.contains("*italic*"));
        assert!(imported.markdown.contains("- one"));
        assert!(imported.markdown.contains("- two"));
    }

    #[test]
    fn rtf_roundtrips_text() {
        let doc = Document::from_text("Rtf manuscript");
        let rtf = craft_rtf::doc::export(&doc);
        let imported = to_markdown(Format::Rtf, rtf.as_bytes()).expect("read rtf");
        assert!(imported.markdown.contains("Rtf manuscript"));
    }

    #[test]
    fn odt_roundtrips_text() {
        let doc = Document::from_text("Odt manuscript");
        let bytes = craft_odf::text::export(&doc).expect("write odt");
        let imported = to_markdown(Format::Odt, &bytes).expect("read odt");
        assert!(imported.markdown.contains("Odt manuscript"));
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
        let imported = to_markdown(Format::Epub, &bytes).expect("read epub");
        assert!(imported.markdown.contains("# One"));
        assert!(imported.markdown.contains("Chapter text."));
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
    fn ordinary_links_and_images_survive() {
        let input = "A [link](https://example.com) and ![alt](photo.png).";
        let (markdown, dropped) = strip_picture_placeholders(input);
        assert_eq!(dropped, 0);
        assert_eq!(markdown, input);
    }

    #[test]
    fn plain_text_passes_through_untouched() {
        let imported = to_markdown(Format::Text, b"# Not Markdown? It is.").expect("text");
        assert_eq!(imported.markdown, "# Not Markdown? It is.");
        assert_eq!(imported.pictures_dropped, 0);
    }

    #[test]
    fn unknown_names_fall_through() {
        assert_eq!(sniff("photo.png", b"\x89PNG\r\n\x1a\n"), None);
        assert_eq!(sniff("archive.zip", b"PK\x03\x04"), None);
    }

    #[test]
    fn broken_manuscript_is_an_error_not_a_panic() {
        let err = to_markdown(Format::Docx, b"not a zip").unwrap_err();
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
        let imported = to_markdown(format, &bytes).expect("import fixture");
        println!(
            "{}: {} chars, {} pictures dropped\n{}",
            format.label(),
            imported.markdown.chars().count(),
            imported.pictures_dropped,
            imported.markdown
        );
    }
}
