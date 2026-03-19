# ripgit

A self-hostable git remote backed by Cloudflare Durable Objects.

One Durable Object per repository. SQLite for storage. [xpatch](https://github.com/ImGajeed76/xpatch) for delta compression. FTS5 for full-text search. Built with [workers-rs](https://github.com/cloudflare/workers-rs) (Rust compiled to WASM).

```
git remote add ripgit https://your-worker.workers.dev/repo/myproject
git push ripgit main
git clone https://your-worker.workers.dev/repo/myproject
```

## Why

Git hosting doesn't need to be complicated. ripgit gives you a git remote with a built-in web UI, diff engine, full-text search, and read API, running entirely on Cloudflare's edge infrastructure. No servers, no containers, no managed databases.

Each repository lives in its own Durable Object with a 10 GB SQLite database. File content is delta-compressed using xpatch, giving compression ratios of 5x+ on real-world repositories. The relational schema makes commit history, file trees, and repository analytics queryable via a simple HTTP API.

## Features

- **Standard git remote** -- `git push`, `git clone`, `git fetch` work out of the box
- **Delta compression** -- xpatch groups blobs by file path and stores forward deltas, with keyframes every 50 versions. Keyframes are zlib-compressed before storage, and chunked across multiple rows if they exceed the 2 MB DO SQLite row limit. Compression ratios of 20x+ on real repos.
- **Streaming pack parser** -- processes git pack files with O(1) memory per object. Builds a lightweight index (offsets + type metadata), then resolves entries on-demand from the pack bytes with a bounded LRU cache for shared delta chain bases. Handles packs with 13K+ objects comfortably within DO's 128 MB memory limit.
- **Web UI** -- server-rendered HTML pages: file browser, commit log, commit detail with colored diffs, search, syntax highlighting with line number anchors, branch selector, markdown README rendering
- **Diff engine** -- recursive tree comparison (short-circuits on matching subtree hashes), line-level unified diffs via `similar` crate, commit-vs-parent and two-commit comparison
- **Full-text search** -- FTS5 indexes file content at HEAD with incremental rebuild (only re-indexes changed files). Auto-detects symbol-heavy queries and falls back to exact substring matching via INSTR. Scope filters by path prefix and file extension. Separate commit message search.
- **Read API** -- browse refs, commit log, trees, file content, diffs, comparisons, search, and compression stats over HTTP
- **One DO per repo** -- strict isolation, no cross-repo interference, scales horizontally

## Web UI

Visit `/repo/:name/` in a browser to get server-rendered HTML pages:

| Page | URL | Description |
|---|---|---|
| Home | `/repo/:name/` | File tree + README (markdown rendered) + recent commits + branch selector |
| Tree | `/repo/:name/tree/:ref/*path` | Directory listing with breadcrumb and parent dir link |
| File | `/repo/:name/blob/:ref/*path` | Syntax highlighted file with line numbers and `#L` anchors |
| Log | `/repo/:name/log?ref=main` | Paginated commit history (30/page) with branch selector |
| Commit | `/repo/:name/commit/:sha` | Commit metadata + full colored diff with hunk headers |
| Search | `/repo/:name/search?q=TODO` | Code tab (all matches with line numbers) + Commits tab |

Content negotiation: requests with `Accept: application/json` get JSON responses; browsers get HTML.

## API

All endpoints are under `/repo/:name/`.

| Endpoint | Description |
|---|---|
| `GET /refs` | List branches and tags |
| `GET /log?ref=main&limit=50` | Commit history (first-parent walk) |
| `GET /commit/:hash` | Single commit detail |
| `GET /tree/:hash` | Directory listing |
| `GET /blob/:hash` | Raw file content |
| `GET /file?ref=main&path=src/lib.rs` | File at ref + path |
| `GET /search?q=TODO&limit=20` | Code search (FTS5 + INSTR fallback) |
| `GET /search?q=fix&scope=commits` | Commit message search |
| `GET /search?q=parse&path=src/&ext=rs` | Scoped code search (path prefix + extension) |
| `GET /diff/:sha` | Diff of a commit against its parent |
| `GET /compare/base...head` | Two-commit comparison diff |
| `GET /stats` | Repository and compression statistics |
| `DELETE /` | Delete all data (for testing) |

Example:

```
$ curl https://your-worker.workers.dev/repo/myproject/stats
{
  "commits": 235,
  "blobs": 1171,
  "blob_groups": 388,
  "database_size_bytes": 8761344,
  "storage": {
    "raw_bytes": 27753426,
    "stored_bytes": 5203978,
    "compression_ratio": 5.33,
    "keyframes": 388,
    "deltas": 783
  }
}
```

## Architecture

```
git push / browser
  |
  v
Worker (entry point, routing)
  |
  | stub.fetch_with_request(req)
  v
Durable Object (one per repo)
  |
  |-- schema.rs    11 tables + 3 FTS5 virtual tables + indexes
  |-- pack.rs      streaming pack parser, resolve cache, pack generator
  |-- git.rs       smart HTTP protocol (receive-pack, upload-pack)
  |-- store.rs     xpatch delta compression, commit graph (binary lifting),
  |                tree walking, blob reconstruction, incremental FTS rebuild
  |-- api.rs       read API endpoints
  |-- diff.rs      recursive tree diff, line-level diffs (via similar)
  |-- web.rs       server-rendered HTML (6 pages), branch selector, markdown
  v
SQLite (10 GB per DO)
```

### Storage model

Commits and trees are stored both parsed (relational tables for querying) and raw (for byte-identical fetch). Blobs are grouped by file path and delta-compressed via xpatch -- only the differences between versions are stored. A full keyframe is stored every 50 versions to bound reconstruction cost.

Keyframes are zlib-compressed before storage, typically reducing a 5 MB source file to ~500 KB. If a compressed keyframe still exceeds 2 MB (the DO SQLite row limit), it's automatically chunked across a `blob_chunks` overflow table and reassembled transparently on read. Deltas are left uncompressed since xpatch already uses zstd internally.

### Streaming pack parser

The pack parser uses a two-pass approach to keep memory usage constant regardless of pack size:

1. **Index pass** -- walks the pack byte stream, decompressing to a sink (discarding data) just to record entry boundaries. Produces a lightweight `Vec<PackEntryMeta>` (~100 bytes per entry) with offsets, types, and delta base references.

2. **Process pass** -- entries are resolved on-demand by type (commits first, then trees, then blobs). Each resolution decompresses from the pack bytes (still in memory as the request body) and walks the delta chain iteratively. A bounded `ResolveCache` (1024 entries) caches resolved bases and intermediates to avoid redundant decompression of shared delta chains -- critical for git packs with depth-50 chains where hundreds of objects share common bases.

Peak memory: pack data (request body) + one resolved object + cache (~20-30 MB). A 13,464-object pack that previously OOM'd at 128 MB now processes comfortably.

### SQL optimizations

Tree entry INSERTs are batched 25 per statement (4 params each, staying under DO's 100 bound parameter limit). Commit parents are batched similarly. Existence checks use `SELECT 1 ... LIMIT 1` instead of `SELECT COUNT(*)`. Blob storage tracks `stored_size` at INSERT time so the stats endpoint uses `SUM(stored_size)` over an integer column instead of `SUM(LENGTH(data))` which would scan every data page.

### Commit graph

A binary lifting table enables O(log N) ancestor queries for merge-base calculations and push validation.

### Search

FTS5 indexes file content at HEAD, rebuilt incrementally on each push using the diff engine to detect changed files. Symbol-heavy queries (containing `.`, `_`, `()`, `::`) automatically fall back to exact substring matching via `INSTR`. Commit messages are indexed in a separate FTS5 table. Files over 1 MiB are skipped for FTS (safely under the 2 MB row limit and unlikely to be hand-written code).

### Default branch

The first branch pushed becomes the default. Stored in a `config` table. HEAD symref is advertised so `git clone` checks out the correct branch.

### Encoding tolerance

Commit messages are parsed with lossy UTF-8 conversion (`String::from_utf8_lossy`), so old repositories with Latin-1 or other non-UTF-8 author names are handled gracefully. The raw bytes are preserved in `raw_objects` for byte-identical fetch.

## Setup

Prerequisites: Rust, [wrangler](https://developers.cloudflare.com/workers/wrangler/), and LLVM (for compiling zstd to WASM).

```bash
brew install llvm
```

Clone and deploy:

```bash
git clone https://github.com/your-org/ripgit
cd ripgit
npx wrangler deploy
```

The `.cargo/config.toml` is pre-configured to point `CC_wasm32_unknown_unknown` at Homebrew's LLVM for the zstd-sys build.

Build locally:

```bash
cargo build --target wasm32-unknown-unknown --release
```

## Pushing large repositories

Cloudflare Workers has a 100 MB request body limit and DOs have a ~30 second storage operation timeout. For repositories whose full pack exceeds these limits, push incrementally using checkpoint commits:

```bash
# Push in 500 first-parent commit increments
STEP=250
TOTAL=$(git rev-list --first-parent --count main)
for FP in $(seq $STEP $STEP $TOTAL); do
  SHA=$(git rev-list --reverse --first-parent main | sed -n "${FP}p")
  git push ripgit "${SHA}:refs/heads/main"
done
git push ripgit main  # final push to HEAD
```

Each incremental push only sends objects the server doesn't have (git negotiates automatically). A 13,464-object repository (`cloudflare/agents`) pushes in a single shot. Larger repos like `git/git` (80K commits) need checkpoint pushes.


## Known limitations

This is a working proof of concept, not production software.

- **No auth** -- anyone with the URL can push and read. Add Cloudflare Access or token auth for real use.
- **DO storage timeout** -- pushes with many objects (>~10K per push) can exceed the DO's ~30 second storage operation timeout. Push large repos incrementally. Cloudflare's `transactionSync()` API would provide atomicity but is not yet exposed in workers-rs.
- **100 MB request body limit** -- a hard Workers platform constraint. Repos whose single-push pack exceeds this must be pushed incrementally via checkpoint commits.
- **No force push** -- untested and may produce inconsistent state.
- **No annotated tags** -- silently dropped during push. Lightweight tags work fine.
See [TODOS.md](TODOS.md) for the full list of completed work, known limitations, and potential future work.

## License

AGPL-3.0. See [LICENSE](LICENSE).

## Acknowledgments

Inspired by [pgit](https://github.com/ImGajeed76/pgit), a PostgreSQL-backed git implementation built on the [xpatch](https://github.com/ImGajeed76/xpatch) delta compression library. ripgit adapts the core ideas for Cloudflare's edge infrastructure using [workers-rs](https://github.com/cloudflare/workers-rs).
