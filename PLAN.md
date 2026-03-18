# ripgit — build plan

A git remote backed by Cloudflare Durable Objects. One DO per repository,
SQLite for storage, xpatch for delta compression, FTS5 for search.

Users interact with it like any other remote:

```
git remote add ripgit https://ripgit.<account>.workers.dev/repo/myproject
git push ripgit main
git clone https://ripgit.<account>.workers.dev/repo/myproject
```

Value over a bare git host: SQL-queryable repo structure, full-text search
on content, compression analytics, HTTP read API — all on cheap, self-hostable
Cloudflare Workers infrastructure.

---

## Current state

PoC in `src/lib.rs` proves:
- xpatch (with zstd) compiles to `wasm32-unknown-unknown` inside workers-rs
- DO with SQLite storage works
- Delta compression + reconstruction via keyframe/forward-delta chains works
- BLOB columns work for binary data
- Cursor iterator avoids holding all deltas in memory during reconstruction

The PoC's `content_blocks` table and `/store`, `/retrieve`, `/stats` endpoints
will be replaced by the real schema and git protocol endpoints below.

---

## Code structure

```
src/
  lib.rs        Worker entry point, DO struct, top-level routing
  schema.rs     All CREATE TABLE / CREATE INDEX statements
  pack.rs       Pack file parsing (push) and generation (fetch)
  git.rs        Smart HTTP protocol: receive-pack, upload-pack
  store.rs      Storage layer: blobs (xpatch), commits, trees, refs, commit graph
  api.rs        Read API: log, tree, file-at-ref, search, stats
```

Split further into submodules when any file exceeds ~500 lines.

---

## Step 1 — Schema

**Goal**: Replace the PoC's `content_blocks` table with the full relational
schema. All tables created in `schema.rs`, called from `DurableObject::new`.

### Tables

```sql
-- Branch and tag pointers
CREATE TABLE IF NOT EXISTS refs (
    name        TEXT PRIMARY KEY,  -- 'refs/heads/main', 'refs/tags/v1.0'
    commit_hash TEXT NOT NULL
);

-- Commit metadata
CREATE TABLE IF NOT EXISTS commits (
    hash            TEXT PRIMARY KEY,  -- hex SHA-1
    tree_hash       TEXT NOT NULL,
    author          TEXT NOT NULL,
    author_email    TEXT NOT NULL,
    author_time     INTEGER NOT NULL,  -- unix epoch seconds
    committer       TEXT NOT NULL,
    committer_email TEXT NOT NULL,
    commit_time     INTEGER NOT NULL,
    message         TEXT NOT NULL
);

-- Commit parent edges (supports merges via ordinal)
CREATE TABLE IF NOT EXISTS commit_parents (
    commit_hash TEXT NOT NULL,
    parent_hash TEXT NOT NULL,
    ordinal     INTEGER NOT NULL,  -- 0 = first parent, 1+ = merge parents
    PRIMARY KEY (commit_hash, ordinal)
);

-- Binary lifting table for O(log N) ancestor queries.
-- Level k stores the 2^k-th ancestor of a commit.
-- Populated during push; enables "is A ancestor of B?" in O(log N) lookups.
CREATE TABLE IF NOT EXISTS commit_graph (
    commit_hash  TEXT NOT NULL,
    level        INTEGER NOT NULL,
    ancestor_hash TEXT NOT NULL,
    PRIMARY KEY (commit_hash, level)
);

-- Tree entries (directory listings)
CREATE TABLE IF NOT EXISTS trees (
    tree_hash  TEXT NOT NULL,
    name       TEXT NOT NULL,     -- entry name (file or subdir)
    mode       INTEGER NOT NULL,  -- 100644, 100755, 040000, 120000
    entry_hash TEXT NOT NULL,     -- blob hash or child tree hash
    PRIMARY KEY (tree_hash, name)
);

-- Delta compression groups for blobs.
-- Blobs with the same path across commits share a group.
CREATE TABLE IF NOT EXISTS blob_groups (
    group_id        INTEGER PRIMARY KEY AUTOINCREMENT,
    path_hint       TEXT,           -- e.g. 'src/lib.rs', used for grouping heuristic
    latest_version  INTEGER NOT NULL DEFAULT 0
);

-- Blob content, delta-compressed via xpatch within each group.
CREATE TABLE IF NOT EXISTS blobs (
    blob_hash        TEXT PRIMARY KEY,
    group_id         INTEGER NOT NULL REFERENCES blob_groups(group_id),
    version_in_group INTEGER NOT NULL,
    is_keyframe      INTEGER NOT NULL DEFAULT 0,
    data             BLOB NOT NULL,     -- xpatch keyframe or delta
    raw_size         INTEGER NOT NULL,  -- original uncompressed size
    UNIQUE (group_id, version_in_group)
);

-- FTS5 full-text index on HEAD content (rebuilt on push to default branch)
-- Created as a virtual table:
-- CREATE VIRTUAL TABLE IF NOT EXISTS fts_head USING fts5(path, content);
```

### Indexes

```sql
CREATE INDEX IF NOT EXISTS idx_commits_time ON commits(commit_time DESC);
CREATE INDEX IF NOT EXISTS idx_commit_parents_parent ON commit_parents(parent_hash);
CREATE INDEX IF NOT EXISTS idx_trees_entry ON trees(entry_hash);
CREATE INDEX IF NOT EXISTS idx_blobs_group ON blobs(group_id, version_in_group);
```

### Notes

- `commit_graph` binary lifting is populated during push. For each new commit,
  level 0 = its first parent. Level k = the level k-1 ancestor of its level k-1
  ancestor. This allows "is commit A an ancestor of commit B?" in O(log N) by
  lifting B up to A's depth and comparing.

- Blob grouping: during push, when we encounter a blob at path `src/lib.rs`, we
  look up `blob_groups` for an existing group with that `path_hint`. If found,
  we delta-encode against the group's latest blob. If not, we create a new group
  and store a keyframe. This gives us pgit-style per-file delta chains.

- FTS5: we only index the HEAD of `refs/heads/main` (or whatever the default
  branch is). On each push that updates the default branch, we rebuild the index.
  This keeps the FTS table small while giving repo-wide content search.

---

## Step 2 — Pack file parser

**Goal**: A `pack.rs` module that can parse a git pack stream (as received
during `git push`) and yield individual objects.

### What git sends on push

The smart HTTP push body contains:
1. **Command lines**: one per ref update, format:
   `<old-hex> <new-hex> <refname>\n`, terminated by a flush packet.
2. **Pack data**: the raw pack stream.

Pack stream format:
```
"PACK"
<version: u32 big-endian>  (always 2)
<num_objects: u32 big-endian>
[object]*
<20-byte SHA-1 of everything before it>
```

Each object:
```
<type_and_size: variable-length encoding>
  type = bits 6-4 of first byte (1=commit, 2=tree, 3=blob, 4=tag,
                                  6=ofs_delta, 7=ref_delta)
  size = remaining bits, extended by continuation bytes
<zlib-compressed data>
  For ofs_delta: preceded by a negative offset to the base object
  For ref_delta: preceded by a 20-byte base object hash
```

### Dependencies

- `flate2` with `miniz_oxide` backend (pure Rust zlib, WASM-safe)
- `sha1` crate (pure Rust, WASM-safe)

### Implementation outline

```rust
pub struct PackObject {
    pub obj_type: ObjectType,  // Commit, Tree, Blob, Tag
    pub hash: String,          // hex SHA-1
    pub data: Vec<u8>,         // fully resolved, decompressed content
}

pub enum ObjectType { Commit, Tree, Blob, Tag }

/// Parse a pack stream, resolve all deltas, yield fully materialized objects.
pub fn parse_pack(data: &[u8]) -> Result<Vec<PackObject>>
```

Git's delta format (OFS_DELTA / REF_DELTA) must be resolved during parsing.
These are a simple copy/insert instruction stream:

```
<base_size: varint>
<result_size: varint>
[instruction]*
  bit 7 = 1: copy from base (next bytes encode offset + length)
  bit 7 = 0: insert literal (bits 0-6 = length, followed by that many bytes)
```

The SHA-1 hash is computed as: `sha1("{type} {size}\0{data}")` where type is
the string "commit", "tree", "blob", or "tag".

### Testing

This can be unit-tested outside the DO by generating pack files with
`git pack-objects` and verifying our parser extracts the same objects.

---

## Step 3 — git-receive-pack (push)

**Goal**: Support `git push` by implementing the smart HTTP receive-pack
protocol.

### Endpoints

**`GET /repo/:name/info/refs?service=git-receive-pack`**

Ref advertisement. Returns current refs so git knows what the remote has.
Response format (pkt-line encoded):

```
001e# service=git-receive-pack\n
0000
<pkt-line: <hash> <refname>\0<capabilities>\n>
<pkt-line: <hash> <refname>\n>  (one per ref)
0000
```

If the repo is empty (no refs), send a single line with the zero hash and
`capabilities^{}` as the ref name.

Capabilities we need: `report-status`, `delete-refs`, `ofs-delta`.

**`POST /repo/:name/git-receive-pack`**

Receives push data. Request body contains:
1. Pkt-line encoded ref update commands
2. A pack file (if any objects are being sent)

Processing:
1. Parse ref update commands (`<old> <new> <refname>`)
2. Parse the pack file via `pack.rs`
3. Store all objects:
   - Commits → `commits` + `commit_parents` + `commit_graph`
   - Trees → `trees`
   - Blobs → find/create `blob_group` by path, xpatch-encode, store in `blobs`
4. Validate: old hash matches current ref value (fast-forward check)
5. Update refs
6. If default branch was updated, rebuild FTS5 index
7. Return report-status response

### Blob path resolution

To group blobs by path for delta compression, we need to know which path
each blob belongs to. During push processing:
1. After parsing all objects, walk each new commit's tree
2. For each blob encountered, record its path
3. Use the path to look up or create a `blob_group`
4. Delta-encode the blob against the latest version in its group

### Pkt-line format

Git's pkt-line encoding: each line is prefixed with 4 hex digits giving the
total line length (including the 4 digits). `0000` is a flush packet.
`0001` is a delimiter. This is simple to implement (~50 lines).

---

## Step 4 — git-upload-pack (fetch / clone)

**Goal**: Support `git clone` and `git fetch` by implementing smart HTTP
upload-pack.

### Endpoints

**`GET /repo/:name/info/refs?service=git-upload-pack`**

Same ref advertisement format as receive-pack, but with
`service=git-upload-pack` header.

**`POST /repo/:name/git-upload-pack`**

Receives want/have negotiation, returns a pack file.

Request body (pkt-line encoded):
```
want <hash> <capabilities>\n
want <hash>\n
...
have <hash>\n
...
done\n
```

Processing:
1. Parse want/have lines
2. Walk the commit graph from wanted commits backward
3. Stop at commits the client already has (the "have" set)
4. Collect all reachable objects (commits, trees, blobs)
5. For blobs: reconstruct full content from xpatch delta chains
6. Generate a pack file containing all needed objects
7. Stream it back as the response body

### Pack generation

For simplicity, generate all objects as non-delta (full) entries. This
produces a larger pack than git would, but is valid and simple:

```rust
pub fn generate_pack(objects: &[PackObject]) -> Vec<u8>
```

Each object: type+size header, then zlib-compressed data. Append SHA-1
checksum at the end. Optimization (adding OFS_DELTA entries) is deferred.

---

## Step 5 — Read API

**Goal**: HTTP endpoints for browsing repository content without git.

### Endpoints

```
GET /repo/:name/refs
  → { "heads": {"main": "<hash>", ...}, "tags": {"v1.0": "<hash>", ...} }

GET /repo/:name/log?ref=main&limit=50&offset=0
  → [{ hash, author, author_email, author_time, message, parents }, ...]

GET /repo/:name/commit/:hash
  → { hash, tree_hash, author, ..., message, parents: [...], diff_stat }

GET /repo/:name/tree/:hash
  → [{ name, mode, hash, type }, ...]   (type = "blob" | "tree")

GET /repo/:name/blob/:hash
  → raw content (reconstructed from delta chain)

GET /repo/:name/file?ref=main&path=src/lib.rs
  → file content at ref (walks tree from commit's tree_hash)

GET /repo/:name/stats
  → { total_commits, total_blobs, db_size_bytes, compression_ratio, ... }
```

### Tree walking

`/file?ref=main&path=src/lib.rs`:
1. Resolve ref → commit hash
2. Get commit's tree_hash
3. Split path by `/`, walk tree entries: `src` → child tree → `lib.rs` → blob hash
4. Reconstruct blob content via xpatch

This is a few SQLite lookups per path segment. Fast enough without caching
for typical path depths (< 10).

---

## Step 6 — FTS5 search

**Goal**: Full-text search across repository content via SQLite FTS5.

### Index structure

```sql
CREATE VIRTUAL TABLE IF NOT EXISTS fts_head USING fts5(path, content);
```

Populated with the content of every text file at the HEAD of the default
branch. Rebuilt on each push that updates the default branch.

### Rebuild process (triggered after push)

1. `DELETE FROM fts_head;`
2. Resolve default branch → commit → tree
3. Recursively walk the tree
4. For each blob: reconstruct content, skip binary files (heuristic:
   check for null bytes in first 8KB)
5. `INSERT INTO fts_head (path, content) VALUES (?, ?);`

### Search endpoint

```
GET /repo/:name/search?q=TODO&limit=20
  → [{ path, snippet }, ...]
```

Uses FTS5's `MATCH` and `snippet()` for highlighted results:

```sql
SELECT path, snippet(fts_head, 1, '<b>', '</b>', '...', 32) as snippet
FROM fts_head
WHERE fts_head MATCH ?
ORDER BY rank
LIMIT ?
```

### Scope control

Only HEAD of the default branch is indexed. This keeps the FTS table small
(proportional to repo size at one point in time, not the full history).
Indexing additional refs (tags, other branches) is a future option.

---

## Build order

| Step | Enables                          | Depends on |
|------|----------------------------------|------------|
| 1    | All subsequent work              | —          |
| 2    | Push (step 3) and fetch (step 4) | —          |
| 3    | `git push`                       | 1, 2       |
| 4    | `git clone` / `git fetch`        | 1, 2       |
| 5    | HTTP browsing API                | 1          |
| 6    | Full-text search                 | 1, 3       |

Steps 1 and 2 can be built in parallel. Step 3 is the first user-visible
milestone. Steps 4, 5, 6 can proceed in any order after their deps.

---

## Open questions

- **Auth**: no auth in v1. Anyone who knows the URL can push/read. Add
  Cloudflare Access or token auth later.
- **Size limits**: DOs have 128MB memory. Large pack files need streaming
  parse, not load-everything-into-memory. The pack parser should operate
  on a stream/slice, not require the full pack in a single `Vec<u8>`.
- **AGPL**: xpatch is AGPL-licensed. Running it as a network service means
  source must be available to users. Decide whether this is acceptable or
  whether to replace xpatch with a permissively-licensed delta library.
- **side-band-64k**: We removed this capability from ref advertisement to
  avoid having to sideband-wrap the report-status response. Should be added
  back later — it enables progress reporting during push/fetch (band 2) and
  structured error messages (band 3). Requires wrapping all response
  pkt-lines with a `\x01` prefix byte (band 1 = primary data).
