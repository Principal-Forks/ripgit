# ripgit

A self-hostable git remote backed by Cloudflare Durable Objects.

One Durable Object per repository. SQLite for storage. [xpatch](https://github.com/ImGajeed76/xpatch) for delta compression. FTS5 for full-text search.

```
git remote add ripgit https://your-worker.workers.dev/repo/myproject
git push ripgit main
git clone https://your-worker.workers.dev/repo/myproject
```

## Why

Git hosting doesn't need to be complicated. ripgit gives you a git remote with a built-in read API and full-text search, running entirely on Cloudflare's edge infrastructure. No servers, no containers, no managed databases.

Each repository lives in its own Durable Object with a 10 GB SQLite database. File content is delta-compressed using xpatch, giving compression ratios of 5x+ on real-world repositories. The relational schema makes commit history, file trees, and repository analytics queryable via a simple HTTP API.

## Features

- **Standard git remote** -- `git push`, `git clone`, `git fetch` work out of the box
- **Delta compression** -- xpatch groups blobs by file path and stores forward deltas, with configurable keyframe intervals
- **Full-text search** -- FTS5 indexes file content at HEAD, searchable via API
- **Read API** -- browse refs, commit log, trees, file content, and compression stats over HTTP
- **One DO per repo** -- strict isolation, no cross-repo interference, scales horizontally

## API

All endpoints are under `/repo/:name/`.

| Endpoint | Description |
|---|---|
| `GET /repo/:name/refs` | List branches and tags |
| `GET /repo/:name/log?ref=main&limit=50` | Commit history (first-parent walk) |
| `GET /repo/:name/commit/:hash` | Single commit detail |
| `GET /repo/:name/tree/:hash` | Directory listing |
| `GET /repo/:name/blob/:hash` | Raw file content |
| `GET /repo/:name/file?ref=main&path=src/lib.rs` | File at ref + path |
| `GET /repo/:name/search?q=TODO&limit=20` | Full-text search with snippets |
| `GET /repo/:name/stats` | Repository and compression statistics |

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
git push
  |
  v
Worker (entry point)
  |
  | stub.fetch_with_request(req)
  v
Durable Object (one per repo)
  |
  |-- schema.rs    8 tables + indexes + FTS5
  |-- pack.rs      git pack parser/generator, OFS_DELTA/REF_DELTA resolution
  |-- git.rs       smart HTTP protocol (receive-pack, upload-pack)
  |-- store.rs     xpatch delta compression, commit graph (binary lifting),
  |                tree walking, blob reconstruction, FTS rebuild
  |-- api.rs       read API endpoints
  v
SQLite (10 GB per DO)
```

**Storage model**: commits and trees are stored both parsed (relational tables for querying) and raw (for byte-identical fetch). Blobs are grouped by file path and delta-compressed via xpatch -- only the differences between versions are stored. A full keyframe is stored every 50 versions to bound reconstruction cost.

**Commit graph**: a binary lifting table enables O(log N) ancestor queries for merge-base calculations and push validation.

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

## Known limitations

This is a working proof of concept, not production software.

- **No auth** -- anyone with the URL can push and read. Add Cloudflare Access or token auth for real use.
- **Memory** -- the entire pack is loaded into memory during push/fetch. Repos with packs exceeding ~100 MB will hit the 128 MB DO memory limit.
- **No force push** -- untested and may produce inconsistent state.

## Acknowledgments

Inspired by [pgit](https://github.com/ImGajeed76/pgit), a PostgreSQL-backed git implementation built on the [xpatch](https://github.com/ImGajeed76/xpatch) delta compression library. ripgit adapts the core ideas for Cloudflare's edge infrastructure using [workers-rs](https://github.com/cloudflare/workers-rs).
