# ripgit — current work and future tasks

## Current state

Steps 1-6 from PLAN.md are implemented and compiling with zero warnings.
Push, clone, fetch, read API, diff engine, FTS5 search (with incremental
rebuild, INSTR fallback, scope filters, commit search), and web UI all work.

Tested with:
- **235-commit repo** — 5.3x compression ratio via xpatch
- **cloudflare/agents** — 13,464 objects, 11.4 MiB pack, pushes in one shot
- **git/git** — 80K commits, pushed incrementally to fp 14,000 (checkpoint pushes)

### Code structure

```
src/
  lib.rs      — Worker entry point, DO struct, routing, ref advertisement
                (HEAD symref), pkt-line encoding, admin endpoints (delete, set-ref)
  schema.rs   — 11 tables + 3 FTS5 virtual tables + indexes
                (refs, commits, commit_parents, commit_graph, trees,
                blob_groups, blobs, blob_chunks, raw_objects, config,
                fts_head, fts_commits)
  pack.rs     — Streaming pack parser: build_index (decompress-to-sink),
                resolve_type (OFS_DELTA chain following), resolve_entry
                (on-demand decompression, Arc-based ResolveCache with byte
                budget, ResolveCtx bundle), pack generator for fetch.
                MAX_PACK_BYTES (50 MB) and CACHE_BUDGET_BYTES (20 MB) constants.
  store.rs    — Storage layer: commit/tree/blob parsing, xpatch delta
                compression with zlib-compressed keyframes + blob_chunks
                overflow, batched SQL INSERTs, binary lifting commit graph,
                tree walking, blob reconstruction, config helpers,
                incremental FTS rebuild, search (FTS5 + INSTR), lossy UTF-8
  git.rs      — Git smart HTTP protocol: receive-pack (pack body size gate,
                streaming pack processing, two-phase push handling, dynamic
                default branch, FTS trigger), upload-pack (fetch)
  api.rs      — Read API: refs, log, commit, tree, blob, file-at-ref,
                search (code + commits, @prefix: column filter syntax),
                stats (using stored_size column, no full table scan)
  diff.rs     — Diff engine: recursive tree comparison, line-level diffs
                (via `similar`), commit diff, two-commit compare
  web.rs      — Web UI: 6 server-rendered HTML pages (home, tree, blob, log,
                commit, search), persistent nav search with live keystroke
                results, branch selector, markdown rendering,
                syntax highlighting, line number anchors
```

---

## Completed

All items below have been implemented and verified.

### Search improvements

1. **All matches with line numbers** — After FTS5 identifies matching files,
   scans content line-by-line and returns every match with its line number.
   Web UI links to `/blob/:ref/:path#L47`.

2. **Literal / exact substring search** — Auto-detects symbol-heavy queries
   (`.`, `_`, `()`, `::`) and falls back to `INSTR(content, ?)`. Full table
   scan but bounded by repo size. `lit:` prefix also forces literal mode.

3. **Scope filters / @prefix: syntax** — `@path:src/`, `@ext:rs`, `@author:`,
   `@message:`, `@content:` inline query prefixes replace separate form fields.
   Parsed in `api::parse_search_query`, strips `@` and maps to FTS5 column
   filters or SQL LIKE predicates. Auto-routes scope (code vs commits) from
   the prefix used. Works for both FTS5 and INSTR modes.

4. **Commit message search** — `fts_commits` FTS5 table indexed on hash,
   message, author. Populated during push. Exposed via `?scope=commits` and
   a tab in the web UI search page.

5. **Incremental FTS rebuild** — Uses `diff::diff_trees` to compare old and
   new HEAD trees. Only inserts/deletes/updates changed files in `fts_head`.
   Stores last indexed commit hash in `config` table. O(changed files) per push.

6. **Default branch detection** — First branch pushed becomes the default.
   Stored in `config` table. FTS rebuild triggers on any push to the default
   branch (not hardcoded to `main`).

### Web UI

7. **Branch selector** — Dropdown on home, tree, blob, and log pages listing
   all `refs/heads/*` entries. Shows current branch prominently. Selecting a
   branch navigates to the same page on that branch.

### Protocol fixes

8. **HEAD symbolic ref** — `advertise_refs` includes `HEAD` pointing to the
   default branch via `symref=HEAD:refs/heads/:name` capability. Fixes
   `git clone` for repos whose default branch isn't `main`.

9. **Clone with non-main branch** — Consequence of #8. `git clone` now checks
   out the correct branch automatically.

10. **Two-phase push handling** — When `git push` sends a payload larger than
    `http.postBuffer` (1 MiB default), git sends a 4-byte probe (`0000`) then
    the full payload with chunked encoding. Fixed by returning 200 OK for
    empty command sets.

11. **Markdown rendering** — Hand-rolled renderer: headings, code blocks,
    lists, blockquotes, bold, italic, inline code, links, horizontal rules.
    Used for README display on the home page.

12. **Syntax highlighting** — highlight.js CDN with line numbers plugin.
    `#L` anchor support for deep linking to specific lines.

### Performance + scale (new)

13. **Streaming pack parser** — Replaced the all-in-memory `parse()` with a
    two-pass approach: index pass (decompress-to-sink, ~100 bytes/entry) then
    process-by-type (decompress on-demand from pack bytes). Peak memory went
    from >128 MiB (OOM) to ~15 MiB for a 13K-object pack.

14. **Resolve cache** — Bounded 1024-entry cache for resolved pack entries.
    Caches delta chain bases and intermediates to avoid re-decompressing shared
    bases. Critical for git packs with depth-50 chains — reduces decompressions
    by 5-10x for packs with many objects sharing base chains.

15. **Keyframe compression** — Keyframes (full blob snapshots, every 50
    versions) are zlib-compressed before storage. A 5 MB source file compresses
    to ~500 KB. Deltas are left as-is (xpatch uses zstd internally). Zero cost
    for the common case (all files fit in single rows after compression).

16. **Blob chunking** — `blob_chunks` overflow table for compressed keyframes
    that still exceed DO's 2 MB row limit. Transparent to all read paths —
    `reconstruct_blob` reassembles chunks automatically. Only activates for
    large binary files.

17. **Batched SQL INSERTs** — Tree entries batched 25 per statement (4 params
    each, under DO's 100 bound parameter limit). Commit parents batched 33 per
    statement. Cuts total SQL operations by ~6x for large pushes.

18. **Fast existence checks** — Replaced `SELECT COUNT(*) AS n` with
    `SELECT 1 LIMIT 1` for dedup checks in store_commit, store_tree,
    store_blob. Indexed PK lookup, instant return.

19. **stored_size column** — Tracks compressed blob size at INSERT time.
    Stats endpoint uses `SUM(stored_size)` over an integer column instead of
    `SUM(LENGTH(data))` which would scan every data page. Instant stats
    regardless of repo size.

20. **Lossy UTF-8** — `String::from_utf8_lossy` for commit parsing. Old repos
    with Latin-1 or other non-UTF-8 author names are handled gracefully.
    Raw bytes preserved in `raw_objects` for byte-identical fetch.

21. **Admin endpoints** — `DELETE /repo/:name/` wipes all tables.
    `PUT /repo/:name/admin/set-ref` manually sets a ref for recovery from
    partial push timeouts.

22. **Persistent nav search with live results** — Search bar in the nav on
    every page. Fetches `/search?q=...` on each keystroke (200ms debounce),
    shows a dropdown of file paths + first matching line (code) or commit hash
    + message (commits). Enter navigates to the full search page. Scope
    (code vs commits) detected client-side from `@author:`/`@message:` prefixes.

23. **Arc-based zero-copy resolve cache** — `ResolveCache` stores `Arc<[u8]>`
    instead of `Vec<u8>`. Cache hits return `Arc::clone` (pointer increment,
    no data copy). `ExternalObjects` also uses `Arc<[u8]>`. `resolve_entry`
    returns `Arc<[u8]>` — each decompressed object is allocated exactly once
    and shared between the cache and the caller. During a processing loop,
    the Arc is at refcount 2 (cache + caller); caller drops at end of iteration,
    leaving refcount 1 in cache. `cache.clear()` drops the last reference.

24. **Budget enforcement** — `MAX_PACK_BYTES = 50 MB` hard gate in
    `handle_receive_pack`: packs above this return a proper `ng` pkt-line
    response before any object is parsed. `CACHE_BUDGET_BYTES = 20 MB`
    enforced inside `ResolveCache::try_cache` — cache silently stops growing
    when the byte budget is exhausted; processing continues via re-decompression.
    Peak memory ceiling at a 50 MB push: ~85 MB (40 MB below the 128 MB wall).
    `ResolveCtx` bundles cache + external objects for `resolve_entry`.

---

## Known limitations

These are documented, accepted trade-offs — not bugs.

- **No auth** — Anyone with the URL can push/read. Add Cloudflare Access or
  bearer token auth for real use.
- **DO storage timeout** — Pushes with many objects (>~10K per incremental
  push) can exceed the DO's ~30 second storage operation timeout. Each
  `sql.exec()` auto-commits individually (no request-level transaction).
  Cloudflare's `transactionSync()` API would provide atomicity but is not
  exposed in workers-rs 0.7.5. Use the admin/set-ref endpoint to recover
  from partial push state.
- **50 MB pack body limit (server-enforced)** — `MAX_PACK_BYTES` in `pack.rs`
  rejects packs above 50 MB with a clean `ng` response before any object is
  parsed. The hard Workers platform limit is 100 MB, but we gate lower to keep
  peak DO memory well under the 128 MB ceiling. Repos must be pushed
  incrementally via the push script's checkpoint mechanism.
- **No force push handling** — Untested; may corrupt state or produce
  confusing errors.
- **No annotated tag objects** — Silently dropped during push. Lightweight
  tags (refs) work fine.
- **Timezone lost in parsed commits table** — The `commits` table stores unix
  timestamps without timezone offset. The `raw_objects` table preserves the
  original bytes for fetch, but the `/log` API shows times without timezone.
- **side-band-64k** — Removed from advertised capabilities to avoid wrapping
  the report-status response with sideband bytes.


---

## Next up

### 1. File history

Show all commits that touched a specific file. Walk the first-parent commit
chain, resolve the file path in each commit's tree, emit a result when the
blob hash changes. O(commits * path_depth) — fast for DO-sized repos.

- **API**: `GET /history?ref=main&path=src/lib.rs` — returns list of commits
  that modified the file, with timestamps, authors, and messages.
- **Web UI**: linked from the blob viewer. Paginated commit list scoped to
  one file.

### 2. Blame

Attribute each line of a file to the commit that last modified it. Leverages
blob_groups (all versions of a file by path) and the diff engine (line-level
diffs).

- **API**: `GET /blame?ref=main&path=src/lib.rs` — returns lines with commit
  hash, author, timestamp per line.
- **Web UI**: blame view linked from the blob viewer. Line numbers, commit
  info column, file content.

### 3. Tags page

Browse lightweight tags in the web UI. Already stored in the `refs` table as
`refs/tags/*`. Quick win — just a new page listing tags with their target
commit info.

- **Web UI**: `/repo/:name/tags` — list of tags with commit hash, author,
  date, and message. Link to commit detail page.

---

## Potential future work

- **transactionSync binding** — Add a custom wasm_bindgen binding for
  `ctx.storage.transactionSync()` to get atomic push semantics. Prevents
  partial state on DO timeout. The JS API exists, workers-rs just doesn't
  expose it yet.
- **Repository index** — KV side-index written on push, landing page listing
  all repos with stats. Needs a KV binding in `wrangler.toml`.
- **Annotated tags** — Parse and store tag objects (separate from lightweight
  tag refs). Requires a `tag_objects` table + pack parser changes.
- **Auth** — Bearer token or Cloudflare Access integration.
- **Force push** — Detect non-fast-forward pushes, handle ref rewrites safely.
- **Streaming zlib compression** — Currently `blob_zlib_compress` buffers the
  entire compressed output (2x blob size in memory). Switching to
  `flate2::write::ZlibEncoder` with incremental chunk writes would eliminate
  the compressed copy, reducing peak memory from `raw + compressed` to
  `raw + ~256 KB`. Compression ratio is identical (single continuous zlib
  stream). Main blocker: interleaves compression with `blob_chunks` INSERT
  logic, changing the `store_blob` flow. Worth doing for medium-sized blobs
  (10-50 MB) where the compressed copy is significant.
- **side-band-64k** — Re-add with proper sideband wrapping for progress
  reporting.
- dont use fetch from DO. expose rpc methods, let worker call the right one.
