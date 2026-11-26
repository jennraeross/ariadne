# craft-libs integration

What Ariadne takes from [storytold/craft-libs](https://github.com/storytold/craft-libs),
in the order the user-visible value is highest. Started 2026-11-26 (Ariadne 0.9.2).

**Pin:** `rev = "39c7c75c52aca67716e5b4bb46dd1196ae1943cc"` (craft-libs `main`).
There is no `v0.1.0` tag on the remote despite the README saying so, so a commit
pin is the only option. Update every craft-libs dependency together, then
`cargo check` and commit the refreshed `Cargo.lock`.

## Rule: headless crates only

craft-libs builds its UI on a **fork of egui 0.37** (`git = storytold/egui`,
pinned by rev). Ariadne is on egui/eframe 0.31 from crates.io. egui types do
not cross versions, so `craft-ui`, `craft-theme`, `craft-text/ui`,
`craft-pages`, `craft-menu`, `craft-dialogs/ui` and `craft-quit/ui` are out
until Ariadne moves to that fork — a whole-app migration, not a dependency.
Everything below is egui-free.

## Ranked list

| # | Improvement | Crates | Status |
|---|---|---|---|
| 1 | **Manuscript import** — drop a `.docx`, `.doc`, `.odt`, `.rtf`, `.html`, `.epub` and get a sheet | `craft-docx`, `craft-doc-binary`, `craft-odf[text]`, `craft-rtf[doc]`, `craft-html[flow]`, `craft-epub`, `craft-markdown`, `craft-flow`, `craft-wordmodel` | **in progress** |
| 2 | **DOCX export that Word understands** — real Heading styles, numbered lists, clickable links, OMML equations, document properties | `craft-docx`, `craft-flow`, `craft-wordmodel`, `craft-math` | todo (approach below) |
| 3 | **EPUB 3 export** — nav document, metadata, cover, per-chapter XHTML | `craft-epub` | todo |
| 4 | **Proofing beyond US English** — dialect choice first, then Hunspell languages and hyphenation | `craft-proof` (Harper dialect is app-side) | todo |
| 5 | **Real RTF tables** instead of space-aligned columns | `craft-rtf[doc]` | todo |
| 6 | **Present → PPTX** — masters, layouts, notes rather than hand-built slides | `craft-pptx` | todo |
| 7 | **Dialogs that don't freeze the window** on slow mounts | `craft-dialogs` (no `ui`) | todo |
| 8 | **ODT export** for LibreOffice users | `craft-odf[text]` | todo |
| 9 | **Full installed-font list** — Font Manager, Adobe Fonts, per-user Windows fonts | `craft-font-catalog` | todo |
| 10 | **Settings backup, recovery, migrations** | `craft-settings` | todo |

Not adopting: `craft-markdown` as the editor parser (subset: no task lists,
no maths), `craft-keymap`, `craft-i18n`, `craft-sniff`, `craft-ipc` (save path
already merges instances), `craft-atomic` (app's `atomic_write` already does
temp + fsync + rename; only directory fsync differs).

## 1. Manuscript import

**Users see:** a `.docx` (or `.doc`, `.odt`, `.rtf`, `.html`, `.epub`) dropped
on the window, or picked from `Import manuscript…`, becomes a sheet in the
current group instead of "Skipped". Today only Markdown/plain text and project
archives are accepted.

**Shape:** each reader returns a `craft_wordmodel::Document`;
`craft_markdown::export` turns that into the Markdown a sheet holds. No layout
or theme mapping is involved, which is what keeps this cheap.

**EPUB structure**

- One sheet per chapter in the book's reading order, collected in a group
  named after the file.
- Spine items marked `nav` or `cover-image` in the manifest, and files named
  `nav`/`toc`/`cover`, are package furniture and are skipped. This is also
  where the "slash symbols all over" report came from: the earlier build
  imported the navigation document, which is nothing but
  `(chapter.xhtml#…)` links.
- A spine item that holds the *whole* book (common: one `section` for the
  lot) is split at its own headings: `h1` when there are two or more; with no
  `h1`, `h2` (two or more), else `h3`. One `h1` followed by `h2`s is a single
  chapter with subsections and stays whole.

**Known limits**

- Pictures are not imported: `craft-markdown` writes `![alt](image.png)` with
  no bytes, so the importer replaces those placeholders with `*[Image: alt]*`
  and reports the count.
- Line breaks survive, the hard-break markers do not: a trailing `\` is
  dropped and a line that is only the marker disappears, so
  Word/PDF-converted books keep their line structure without being
  littered with backslashes.
- Equations are exported as nothing by `craft-markdown` (no LaTeX path yet).
- A heading level used for both chapters and section breaks can split too
  finely; the importer has no author intent to go on.

**Follow-ups:** import pictures as attachments (walk `Document::media`, place
them in order), equations to `$…$`, honour `epub:type` when present.

**Acceptance**

- [x] `.docx`, `.doc`, `.odt`, `.rtf`, `.html`, `.epub` and `.md`/`.txt` import
- [x] Format recognised by extension and by bytes (a misnamed `.docx` works)
- [x] Failures set the status line; nothing panics on a malformed file
- [x] Unit tests round-trip each format through its own writer
- [x] EPUB becomes one sheet per chapter, in a group named after the book
- [x] LibreOffice-authored `.docx` and `.odt` import with headings, emphasis,
      links, inline code, nested lists, ordered lists, quotes and tables
      (`cargo test import::tests::external_fixture -- --ignored`, with
      `ARIADNE_IMPORT_FIXTURE` set)
- [x] `cargo check --target wasm32-unknown-unknown --no-default-features`
- [ ] Word-authored `.docx` by hand (no Word on this machine)

Note: LibreOffice's HTML→DOCX conversion writes a quotation as an indented
paragraph, so it arrives as body text; the ODT of the same file keeps `>`.
Word's Quote style does map.

## 2. DOCX export that Word understands

**Users see:** headings in Word's navigation pane and automatic TOC, real
list numbering, clickable links, equations that stay equations, and document
properties. Today `export_docx_bytes` writes only `word/document.xml`: no
`styles.xml`, no `numbering.xml`, bullets as `•` text, links as `text (url)`,
maths flattened to Unicode.

**Work (revised after reading the exporter):** replace the internals of
`export_docx_bytes` in place rather than swapping in `craft_docx::write` — the
current writer already handles page size/margins, page breaks, images with
drawing relationships and tables, and those are exactly what a rewrite would
regress. The missing pieces are package parts and paragraph structure:

1. `word/styles.xml`: `docDefaults` from the body style, plus named
   `Title`/`Heading1`–`Heading3` (with `w:outlineLvl`) / `Quote` / `Code`
   styles built from the export theme; reference them with `w:pStyle`.
2. `word/numbering.xml`: one bullet list and one decimal list, with a fresh
   `w:num` per contiguous list run so numbering restarts; drop the literal
   `•` and `1.` prefixes.
3. Real `w:hyperlink` runs (`document.xml.rels` per unique URL,
   `TargetMode="External"`) instead of `text (url)`.
4. `docProps/core.xml`: title, creator from settings, created/modified.
5. `$$…$$` through `craft-math` to OMML instead of Unicode flattening.

`craft-docx`/`craft-flow` stay for import and are the reference for the XML
shapes; adopting `craft_docx::write` wholesale is deferred until there is a
theme-to-styles mapping proven against existing exports.

**Acceptance:** Word shows headings in the navigation pane, list numbering
restarts correctly, links open, `$$…$$` stays editable; LibreOffice opens the
file without repair.

## 3. EPUB 3 export

**Users see:** an epub Apple Books and Kindle pipelines accept without
complaints: EPUB 3 package, nav document, creator/language/modified metadata,
cover, one XHTML per sheet with the sheet title as `h1`.

**Work:** replace the hand-built OPF 2.0/NCX package in `export_epub_bytes`
with `craft_epub::Book` + `write`; the chapter XHTML built by
`blocks_html_fragment` stays the content.

## 4. Proofing beyond US English

**Users see:** the right dialect flagged (today `Dialect::American` is hard
coded in `grammar.rs`), then other languages. Harper stays for English
grammar; `craft-proof` adds installed Hunspell dictionaries and hyphenation.

**Acceptance:** a setting chooses the dialect; a non-English language can be
selected, with installed dictionaries found on macOS/Windows/Linux.

## 5. RTF tables

**Users see:** tables that stay tables in Word, Scrivener and LibreOffice;
today they export as space-aligned text.

## 6. Present → PPTX

**Users see:** decks that open cleanly and are editable per shape, with
speaker notes, rather than a hand-built slide XML.

## 7. Dialogs that don't freeze

**Users see:** the window keeps painting while a file panel is open, and slow
network mounts no longer hang the frame. Today `rfd` is called synchronously
inside the update loop.

## 8. ODT export

**Users see:** an `.odt` export target for LibreOffice/OpenOffice users, from
the same word-model mapping built for item 2.

## 9. Installed font list

**Users see:** fonts activated by Font Manager/Adobe Fonts and per-user Windows
fonts in the editor/export font picker. Today `fontdb` finds font folders.

## 10. Settings backup and recovery

**Users see:** a damaged or half-written settings file recovers instead of
resetting, and settings survive upgrades through migrations.

## Verification checklist (every item)

- `cd app && cargo fmt --check && cargo clippy --locked --all-targets && cargo test --locked`
- `cargo check --target wasm32-unknown-unknown` when the code is not native-only
- `cargo-deny` (licenses and advisories): the set so far is MIT/Apache-2.0;
  `chrono` arrives transitively through `craft-docx`, so the unmaintained
  advisory must stay scoped to workspace members (it does)
- Open the produced file in the real app: Word, LibreOffice, Apple Books,
  Calibre, Scrivener

## Change log

- 2026-11-26: plan written; item 1 import wired for docx/doc/odt/rtf/html/epub
  (drop + `Import manuscript…`), 11 unit tests, +45 crates in `Cargo.lock`.
- 2026-11-26: Document hard-break markers stripped while lines stay; the window
  paints an opaque base under the panels so no desktop shows between them.
- 2026-11-26: EPUB import splits chapters (spine items and, for whole-book
  files, top-level headings), in a group named after the file; navigation and
  cover pages skipped. Library footer button added for the manual path.
