# Ulysses integration backlog

Reference list for features worth bringing from Ulysses into Ariadne.
Ordered by priority, then by implementation size. Prerequisites are ordered first.

Effort: **S** small, **M** medium, **L** large.

## Status (this pass)

Implemented: **H1–H9** and **L1–L7**. H7 revision mode covers dimmed text,
the Review tab, and one-click fixes, but there is no separate annotation
store yet.

## High priority

1. **[H1] Physical keyboard navigation** — **S**
   Arrow keys move library → sheets → editor; `Return` drills in; `Cmd/Ctrl+Return`
   goes back up. Especially important for tiled/keyboard-first use.

2. **[H2] Table of contents export** — **S**
   Support a `(toc)` marker (or export option) that builds a TOC from headings.
   Reuses the existing outline parser and export pipeline.

3. **[H3] Creation dates and date correction** — **S**
   Add a created timestamp alongside `modified`; show both in the dashboard and
   allow correcting the created date. *Moved up because creation-date sorting
   and date-based group ordering depend on it.*

4. **[H4] Share project as archive** — **S–M**
   Export the library/project as a zip containing sheets, attachments, history,
   and metadata. Natural backup/share path for a file-based library.

5. **[H5] Group icons, colors, and per-group sorting** — **S–M**
   Icon/color picker for groups, plus manual/title/modified/created sorting per
   group. *Created-date sorting depends on [H3].*

6. **[H6] Project and sheet templates** — **S–M**
   Starter groups/sheets for novel, blog, notes, research, etc. Could reuse a
   template group or project archive as the storage format.

7. **[H7] Revision mode** — **M**
   A focused review state combining grammar/style issues, annotations, and
   dimmed text. Builds on the existing Review tab, focus mode, and markup bar.

8. **[H8] Keyword manager, colors, and keyword favorites** — **M**
   Library-wide keyword panel: rename, merge, delete, color, favorite, filter.
   Builds on the existing per-sheet keyword field.

9. **[H9] Daily goals, deadlines, and goal history** — **M–L**
   Daily word targets, a deadline that calculates required daily output, and a
   compact writing-history chart. Needs a small daily-session metric store;
   history depends on that store.

## Lower priority

1. **[L1] Attachments in Overview** — **S**
   Expand the dashboard media section into image/note thumbnails. Reuses the
   existing attachment registry and image textures.

2. **[L2] Typewriter line/sentence highlighting** — **S–M**
   Options for highlighting the current line or sentence, not just dimming
   paragraphs outside the caret's.

3. **[L3] Internal links** — **L**
   Link to headings in the same sheet or project and jump to them. Required
   before backlinks.

4. **[L4] Backlinks / incoming links** — **L**
   Show sheets that link to the current sheet and jump to each link.
   *Depends on [L3].*

5. **[L5] Split editor** — **L**
   Vertical or horizontal second editor for two texts at once, including
   simultaneous scrolling and active-editor highlighting.

6. **[L6] Tables** — **L**
   Create/edit/format tables and export them to the existing formats.

7. **[L7] LaTeX equations** — **L**
   Inline and block equations, rendered in the editor and exports.
