# ripgit — current work and future tasks

## Current state

Steps 1-6 from PLAN.md are implemented and compiling. Push, clone, read API,
FTS5 search, diff engine, and web UI all work. Tested with a 235-commit,
1171-blob repo (5.3x compression ratio via xpatch).

### Code structure

```
src/
  lib.rs      — Worker entry point, DO struct, routing, ref advertisement, pkt-line encoding
  schema.rs   — All table definitions (refs, commits, commit_parents, commit_graph,
                trees, blob_groups, blobs, raw_objects, fts_head)
  pack.rs     — Git pack file parser (with OFS_DELTA/REF_DELTA resolution) + generator
  store.rs    — Storage layer: commit/tree/blob parsing, xpatch delta compression,
                binary lifting commit graph, tree walking, FTS5 rebuild
  git.rs      — Git smart HTTP protocol: receive-pack (push), upload-pack (fetch)
  api.rs      — Read API: refs, log, commit, tree, blob, file-at-ref, search, stats
  diff.rs     — Diff engine: recursive tree comparison, line-level diffs (via `similar`)
  web.rs      — Web UI: server-rendered HTML pages (home, tree, blob, log, commit, search)
```

---

## NEXT: Search improvements

### 1. All matches with line numbers

**Problem**: FTS5's `snippet()` returns one fragment per file. If a file has
10 matches for "TODO", we only show the first one. No line numbers, so you
can't click through to the exact location.

**Fix**: After FTS5 identifies matching files, scan their content line-by-line
and return every matching line with its line number. Return structured results:
`[{path, matches: [{line_number, line_text}]}]`. Web UI links to
`/blob/:ref/:path#L47`.

**Location**: `src/store.rs` (new function), `src/api.rs` `handle_search`,
`src/web.rs` `page_search`.

### 2. Literal / exact substring search

**Problem**: FTS5 tokenizes on word boundaries. Searching for `fn foo_bar`
becomes `fn AND foo AND bar` (loses the underscore). `.unwrap()` loses the
dot and parens. Symbol-heavy queries (common in code) return wrong results.

**Fix**: Auto-detect queries that contain symbols (`.`, `_`, `(`, `::`, etc.)
and fall back to `INSTR(content, ?)` for exact substring matching. This is
a full table scan, but bounded by repo size — a 50 MB repo scans fine in a
DO's SQLite. Users can also prefix with `lit:` to force literal mode.

**Location**: `src/api.rs` `handle_search`, `src/web.rs` `page_search`.

### 3. Scope filters (path prefix, file extension)

**Problem**: No way to narrow search to a directory or file type.

**Fix**: Add `?path=src/` and `?ext=rs` query params. Apply as `WHERE path
LIKE 'src/%'` and `WHERE path LIKE '%.rs'` filters on the FTS results.
Works for both FTS5 MATCH and INSTR fallback modes.

**Location**: `src/api.rs` `handle_search`, `src/web.rs` `page_search`.

### 4. Commit message search

**Problem**: Only file contents are searchable. Can't search commit history
by message, author, etc.

**Fix**: Add a second FTS5 table `fts_commits USING fts5(hash, message,
author)`. Populated incrementally during push (each new commit gets inserted).
Negligible storage cost — 10,000 commits at 100 bytes/message = ~1 MB raw,
~3 MB with FTS5 overhead. Expose via `?scope=commits` on the search endpoint
and a tab in the web UI search page.

**Location**: `src/schema.rs` (new table), `src/store.rs` `store_commit`
(insert into FTS), `src/api.rs`, `src/web.rs`.

### 5. Incremental FTS rebuild

**Problem**: Current rebuild wipes the entire `fts_head` table and re-indexes
every file on every push to main. O(all files) per push.

**Fix**: Use the diff engine (`diff::diff_trees`) to compare the old and new
trees. Only INSERT new files, DELETE removed files, and UPDATE modified files
in `fts_head`. Requires storing the previous indexed commit hash (one row in
a `config` table or similar). Cuts rebuild to O(changed files) per push.

**Location**: `src/store.rs` `rebuild_fts_index`, `src/git.rs` FTS trigger.

### 6. Fix default branch detection for FTS

**Problem**: FTS rebuild only triggers on `refs/heads/main`. Repos with a
different default branch (e.g. `master`, `osify`) never get indexed.

**Fix**: Track the default branch. On first push, set the default to whatever
branch is pushed first. Store as `HEAD -> refs/heads/:name` in the refs table
or a config table. Trigger FTS rebuild on any push to the default branch.
This also fixes the HEAD symbolic ref bug (see below).

**Location**: `src/git.rs` (FTS trigger), `src/store.rs`, `src/lib.rs`
`advertise_refs`.

---

## NEXT: Web UI — branch navigation

### 7. Branch selector in web UI

**Problem**: The web UI has no way to switch branches. You must manually edit
the URL. The home page always shows the default branch with no indication
that other branches exist.

**Fix**: Add a branch dropdown/selector to the home page, tree browser, blob
viewer, and log page. Show current branch name prominently. Dropdown lists
all `refs/heads/*` entries from the refs table. Selecting a branch navigates
to the same page on that branch.

**Location**: `src/web.rs` (all page functions), `src/lib.rs` routing.

---

## DEFERRED: Bugs found during stress test

### 8. No HEAD symbolic ref

**Problem**: `git clone` warns "remote HEAD refers to nonexistent ref, unable
to checkout" when the only branch isn't `main`. We don't advertise a `HEAD`
symbolic ref in the ref advertisement.

**Fix**: Track a `HEAD` symbolic ref (store `HEAD -> refs/heads/:name` in the
refs table or config). Include it in the ref advertisement. Set it to the
first branch pushed if not configured. Tied to #6.

**Location**: `src/lib.rs` `advertise_refs()`, `src/store.rs`.

### 9. Clone creates no working tree when branch != main

**Consequence of #8**. Once HEAD is advertised correctly, `git clone` will
check out the right branch automatically. No separate fix needed.

---

## LATER: Known limitations

- **No auth**: Anyone with the URL can push/read. Add Cloudflare Access or
  bearer token auth.
- **No force push handling**: Untested; may corrupt state or produce
  confusing errors.
- **No annotated tag objects**: Silently dropped during push. Lightweight
  tags (refs) work fine.
- **Timezone lost in parsed commits table**: The `commits` table stores unix
  timestamps without timezone. The `raw_objects` table preserves the original
  bytes for fetch, but the `/log` API always shows times without timezone
  offset.
- **No error resilience**: A malformed pack could corrupt DO state. No
  transaction rollback on partial failures.
- **side-band-64k**: Removed from capabilities to avoid sideband-wrapping
  the report-status response. Should be added back for progress reporting.
- **Large pack memory**: Both the Worker and DO load the entire pack into
  memory. Repos with packs > ~100 MiB will OOM the 128 MB DO memory limit.
  Needs streaming pack parsing.
- **AGPL license**: xpatch is AGPL-licensed. Running as a network service
  requires source availability. Decide whether to accept or replace with a
  permissively-licensed delta library.
