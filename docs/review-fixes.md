# Ariadne code review — fix plan

Working tracker for the full-codebase review (2026-11-25). Ordered by severity,
then by the order the fixes should land. Keep this file updated as items are
implemented; the short notes at the bottom record deliberate deviations.

**How this was reviewed:** working tree at `d7a6dc4` + uncommitted feature work
(`main.rs` is ~39k lines). `cargo test` passed 254 tests; `cargo clippy` failed
(two denied-by-default lints); `cargo fmt --check` was dirty. The app was run
headlessly for the library/editor/settings/export/present/dashboard and for
failure injection (unwritable library path, corrupt web store).

Legend: **P0** data loss/security · **P1** serious correctness/perf · **P2**
quality gates/maintainability · **P3** smaller issues. Line references point
at the tree as reviewed, before the fixes; the progress log has the outcome.

---

## P0 — data loss and security

- [x] **R1. Surface save failures instead of always claiming "Saved".**
  `save_library` ignores write errors (`main.rs:9097`; `atomic_write` result
  dropped around `:9225`) while the status bar hardcodes a green "Saved"
  (`:31071-31083`). Reproduced with an unwritable library path: the app opens
  samples and still shows "● Saved".
  Fix: track `save_error`; show red "Not saved — <reason>" plus retry status;
  retry automatically; flush on exit; test with an unwritable path.

- [x] **R2. Stop rewriting the whole library on every editing frame.**
  `edited` → `save_library()` on each edit (`:11224`, `:11580`) and on
  navigation (`:7067`, `:5258`). One save clones every sheet, pretty-prints the
  library, fsyncs (`:7675`), re-reads the iCloud store and rewrites/compares
  every mirror file and attachment (`:7853-7935`). Measured release: 21–25 ms
  per save for 400 sheets / 2.9 MB (~12 ms in the mirror).
  Fix: debounce autosaves (~400 ms idle), flush on blur/switch/quit; cache
  mirror file metadata/hashes to avoid re-reading unchanged files; skip saves
  for pure navigation.

- [x] **R3. Key editor widget state by sheet UUID, not list index.**
  `editor_widget_id` uses the index (`:5192`); egui stores cursor/scroll and
  the undo stack (full text snapshots) under that ID. Permanent deletion
  (`:6261`) and drag-reorder (`:7268-7293`) shift indices without clearing it,
  so Ctrl+Z in the sheet that takes the slot can restore the deleted sheet's
  text (which is then autosaved). Use the UUID for editor/find/paste IDs; add
  a regression test.

- [x] **R4. Confirm permanent deletion and empty-trash.**
  "Delete forever" (`:6255`, menu `:30940`) and "Empty Trash" (`:6280`,
  `:30440`) run immediately and also drop the sheet's history
  (`forget_history_above`). Add a small modal (egui `Modal`) for both; keep
  history until confirmed.

- [x] **R5. Preserve a corrupt browser library instead of overwriting it.**
  Native quarantines a corrupt `library.json` (`:9690-9705`); web
  `load_library_web` just returns `None` (`:9565-9578`), and first-run code
  immediately writes samples (`:4236-4237`). Copy the raw slot to a recovery
  key before replacing it, surface a notice, and offer it as a download.

- [x] **R6. Open URLs/files without a shell on Windows.**
  `gdrive::open_url` (`gdrive.rs:699-711`) and `open_with_default_app`
  (`:18842`) pass user-controlled strings to `cmd /C start`; `&` is parsed by
  cmd and can start another command. Use `webbrowser` (already in the lock via
  egui-winit) for URLs and `explorer.exe` for files on Windows; validate
  schemes (http/https/mailto) in the app's link path.

## P1 — correctness, sync, performance

- [x] **R7. Sync sheet metadata and style edits.**
  Merge only compares content/trashed (`:8441`), so favourite/keyword/material
  changes never propagate; `group_styles`/`keyword_styles` merge with
  `or_insert` (`:9388-9391`), so the first device wins forever. Bump
  `modified` on favourite/material toggles and merge metadata with
  newest-wins; add `modified` timestamps to `GroupStyle`/`KeywordStyle` and
  merge those newest-wins too.

- [x] **R8. Make backups restorable and import text files.**
  "Share project archive" writes a `.zip` (`:18460+`) but nothing reads one,
  and dropped files are only tried as images (`:13570`) — so `.md`/`.zip` are
  rejected. Add a stored-ZIP reader + merge import, a palette action (native
  file dialog), and drag-drop for `.md`/`.markdown`/`.txt`/`.zip`.

- [x] **R9. Fix daily metrics: local dates and structural edits.**
  `today_key` is UTC (`:26980`; deadlines `:27016`), so the writing day rolls
  over mid-evening west of UTC. `maybe_track_daily_words` (`:26992`) counts
  positive deltas of `total_library_words`, which excludes trash/material:
  restoring a sheet or un-marking material records its words as written today;
  sync conflicts/imports do too. Use local date keys and reset the baseline
  after structural/sync changes.

- [x] **R10. Remove per-frame full-library rescans.**
  `visible_sheets()` is rebuilt/sorted twice per frame (`:30380`, `:30567`) and
  Title sort recomputes titles (`:5405`); each card recomputes title, preview,
  words, chars (`:30650-30665`); search lowercases whole bodies per sheet per
  frame (`:10763`) and per keystroke in the palette (`:21130`); Overview
  backlinks scan every sheet (`:21368`). Cache card stats, lowered bodies,
  parsed links and backlinks keyed by UUID + `modified`; compute the visible
  list once per frame.

- [x] **R11. Move document exports off the UI thread.**
  `do_export` builds the whole PDF/DOCX/EPUB before painting (`:18369`); only
  presenter export has a worker. Add a worker snapshot + channel (native),
  progress/status, then write/open on completion; keep wasm synchronous.

## P2 — quality gates and maintainability

- [x] **R12. Make clippy/fmt pass and gate them in CI.**
  Hard errors: `never_loop` (`gdrive.rs:738`), `absurd_extreme_comparisons`
  (`main.rs:36984`). `cargo fmt --check` is dirty; CI runs only tests + deny.
  Fix the two lints, run `cargo fmt`, add `cargo clippy --all-targets` and
  `cargo fmt --check` jobs.

- [x] **R13. Start splitting the 39k-line `main.rs` (first extraction done).**
  Extract pure, UI-free units first (export engines/markdown blocks, sync
  merge, themes) so tests stay fast and changes stay reviewable. Done so far:
  `src/archive.rs` holds the ZIP writer/reader. Further splits remain and are
  intentionally kept out of this working batch.

## P3 — smaller real issues

- [x] **R14. Mirror pruning must not delete user files.** `prune_mirror`
  (`:7942`) removes any `.md` under the iCloud store without checking the
  managed header. Only delete files that start with the Ariadne comment.

- [x] **R15. Let users remove unused attachments.** `attached_images` only
  grows; add a "Clean up unused images" command that drops unreferenced
  registry entries and files (native + web store).

- [x] **R16. Merge the local store on save (multi-instance safety).** Two app
  instances clobber local `library.json` because only the iCloud store is
  re-read before saving (`:9130-9230`). Merge the local store too, using the
  existing hash gate.

- [x] **R17. Version the PWA cache per deploy.** `web/sw.js` hardcodes
  `ariadne-app-v1` and installs without precaching, so a deploy can serve a
  stale build for one load and old hashed assets accumulate. Rewrite the cache
  name with the commit SHA in `web.yml`.

- [x] **R18. Count words without Markdown noise.** `counts` (`:4299`) treats
  `#`, list dashes, table pipes and fences as words. Filter tokens with no
  alphanumeric content and strip line prefixes; keep character counts as raw
  source characters.

- [ ] **R19. Small UX polish (deferred).** The export modal's icon-only row
  already has tooltips; adding text labels in the 260px column would crowd the
  layout. The sheets search row is already visible in the current tree. Revisit
  when the export panel is next redesigned.

---

## Progress log

- **2026-11-25:** R1–R18 implemented on the working tree. `cargo test` 268
  passing, `cargo clippy --all-targets` clean (warnings only), `cargo fmt
  --check` clean, and the wasm build (`cargo check --target
  wasm32-unknown-unknown --no-default-features`) compiles. CI now runs fmt +
  clippy and stamps the PWA cache per deploy. First module extraction:
  `src/archive.rs` (ZIP writer/reader). R19 deferred (see above).

## Deliberate deviations / notes

- R1–R6: save health/retry + debounced autosave + metadata-cached mirror,
  UUID-keyed editor/find/paste ids, confirmation modal for permanent deletion,
  web recovery slot + palette download, and shell-free URL/file opening
  (webbrowser on native, explorer.exe on Windows) with scheme checks.
- R7–R11: metadata/style newest-wins sync, archive import + Markdown drop,
  local-date metrics + rebaselining, per-frame stats/search/link caches,
  background document exports on native.
- R12/R14–R18: clippy/fmt CI gates, mirror prune safety, attachment cleanup
  command, local-store merge on save (multi-instance), PWA cache stamping, and
  Markdown-aware word counts.
- Because `cargo fmt` was run as part of R12, the diff against the user's
  existing uncommitted tree is much larger than the behavioral changes; it is
  formatting plus the fixes above.

- The review's "sheets search is undiscoverable" finding was wrong for the
  current tree: the search field is always visible under the pane header; only
  the stale prebuilt release binary lacked it. Dropped from the list.
- "Sheets can't be moved between groups" — no such UI exists (drag reorders
  within a group), so the sync group-move part is only reachable via group
  deletion remaps. Metadata sync (R7) still matters.
- R13 is a structural change that should not be mixed into the existing
  uncommitted feature diff more than a first extraction; it is tracked here so
  it is not lost.
- Measurements from a temporary in-tree benchmark (reverted): release build,
  400 sheets / 2.9 MB, `save_library` 21–25 ms, mirror ~12 ms.
