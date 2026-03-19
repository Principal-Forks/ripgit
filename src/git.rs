//! Git smart HTTP protocol handlers for receive-pack and upload-pack.

use crate::{pack, store, KEYFRAME_INTERVAL};
use std::collections::{HashMap, HashSet};
use worker::*;

// ---------------------------------------------------------------------------
// git-receive-pack (handles `git push`)
// ---------------------------------------------------------------------------

/// Process a git-receive-pack POST request.
///
/// Uses a streaming approach: builds a lightweight index of the pack entries,
/// then processes objects by type (commits → trees → blobs), decompressing
/// each entry on-demand from the pack bytes. Only one resolved object is held
/// in memory at a time, keeping peak memory to pack_size + O(1 object).
///
/// 1. Parse pkt-line ref update commands
/// 2. Build pack index (decompress-to-sink, no data held)
/// 3. Pre-compute types by following OFS_DELTA chains
/// 4. Process commits (decompress, store, drop)
/// 5. Process trees (decompress, store, drop)
/// 6. Resolve blob paths (all trees now in DB)
/// 7. Process blobs (decompress, store with xpatch compression, drop)
/// 8. Update refs
/// 9. Return report-status
pub fn handle_receive_pack(sql: &SqlStorage, body: &[u8]) -> Result<Response> {
    // --- 1. Parse ref update commands from pkt-lines ---
    let (commands, pack_offset) = parse_ref_commands(body);

    // Git splits large pushes (>http.postBuffer) into two POSTs:
    //   1st: a 4-byte flush "0000" (no commands, no pack)
    //   2nd: the full payload (commands + flush + pack)
    // Return 200 for the probe so git proceeds with the real request.
    if commands.is_empty() {
        let mut resp = Response::from_bytes(Vec::new())?;
        resp.headers_mut()
            .set("Content-Type", "application/x-git-receive-pack-result")?;
        return Ok(resp);
    }

    // --- 2-7. Process pack data (streaming) ---
    // Note: Cloudflare DO SQLite does not support BEGIN/COMMIT via sql.exec().
    // transactionSync() is not available in workers-rs 0.7.5.
    // Each sql.exec() auto-commits individually. If the DO times out mid-push,
    // partial state may result. Use the admin/set-ref endpoint to recover.
    // TODO: look into getting transaction support in workers-rs
    let pack_data = &body[pack_offset..];
    if pack_data.len() > 4 && &pack_data[..4] == b"PACK" {
        process_pack_streaming(sql, pack_data)?;
    }

    // --- 8. Update refs ---
    let mut results: Vec<(String, std::result::Result<(), String>)> = Vec::new();

    for cmd in &commands {
        let result = store::update_ref(sql, &cmd.ref_name, &cmd.old_hash, &cmd.new_hash)
            .map_err(|e| format!("{}", e));
        results.push((cmd.ref_name.clone(), result));
    }

    // --- Set default branch + rebuild FTS index ---
    for (ref_name, result) in &results {
        if result.is_ok() && ref_name.starts_with("refs/heads/") {
            if store::get_config(sql, "default_branch")?.is_none() {
                let _ = store::set_config(sql, "default_branch", ref_name);
            }
        }
    }

    if let Some(default_ref) = store::get_config(sql, "default_branch")? {
        for cmd in &commands {
            if cmd.ref_name == default_ref {
                if let Some((_, Ok(()))) = results.iter().find(|(r, _)| r == &cmd.ref_name) {
                    let _ = store::rebuild_fts_index(sql, &cmd.new_hash);
                }
            }
        }
    }

    // --- 9. Return report-status ---
    let status_body = build_report_status(&results);

    let mut resp = Response::from_bytes(status_body)?;
    resp.headers_mut()
        .set("Content-Type", "application/x-git-receive-pack-result")?;
    Ok(resp)
}

/// Process pack data using the streaming two-pass approach.
///
/// Pass 1: `build_index` walks the pack byte stream, recording metadata for
/// each entry (offsets, type, delta base references). Zlib data is decompressed
/// to a sink — no object data is held in memory.
///
/// Pass 2: entries are processed by type. Each is decompressed on-demand from
/// the pack bytes (which stay in memory as the request body), delta chains are
/// resolved iteratively, and the result is stored in permanent tables then
/// dropped. Only one resolved object exists in memory at a time.
fn process_pack_streaming(sql: &SqlStorage, pack_data: &[u8]) -> Result<()> {
    // --- Build lightweight index ---
    let (index, offset_to_idx) = pack::build_index(pack_data).map_err(|e| Error::RustError(e.0))?;

    // --- Pre-compute types by following OFS_DELTA chains ---
    // Returns Some(type) for entries resolvable via OFS_DELTA, None for REF_DELTA.
    let types: Vec<Option<pack::ObjectType>> = (0..index.len())
        .map(|i| pack::resolve_type(&index, &offset_to_idx, i))
        .collect();

    let mut hash_to_idx: HashMap<String, usize> = HashMap::new();

    // Resolve cache: avoids re-decompressing shared delta chain bases.
    // 1024 entries ≈ 20-30 MB, well within DO's 128 MB memory limit.
    let mut cache = pack::ResolveCache::new(1024);

    // --- Process commits ---
    let mut root_tree_hashes: Vec<String> = Vec::new();

    for i in 0..index.len() {
        if types[i] != Some(pack::ObjectType::Commit) {
            continue;
        }
        let (_, data) = pack::resolve_entry(
            pack_data,
            &index,
            &offset_to_idx,
            i,
            &hash_to_idx,
            &mut cache,
        )
        .map_err(|e| Error::RustError(e.0))?;
        let hash = pack::hash_object(&pack::ObjectType::Commit, &data);
        hash_to_idx.insert(hash.clone(), i);
        let parsed = store::parse_commit(&data)
            .map_err(|e| Error::RustError(format!("commit {}: {}", hash, e)))?;
        root_tree_hashes.push(parsed.tree_hash.clone());
        store::store_commit(sql, &hash, &parsed, &data)?;
    }

    // --- Process trees ---
    for i in 0..index.len() {
        if types[i] != Some(pack::ObjectType::Tree) {
            continue;
        }
        let (_, data) = pack::resolve_entry(
            pack_data,
            &index,
            &offset_to_idx,
            i,
            &hash_to_idx,
            &mut cache,
        )
        .map_err(|e| Error::RustError(e.0))?;
        let hash = pack::hash_object(&pack::ObjectType::Tree, &data);
        hash_to_idx.insert(hash.clone(), i);
        store::store_tree(sql, &hash, &data)?;
    }

    // --- Resolve blob paths (all trees now in permanent storage) ---
    let empty_pack_trees: HashMap<String, Vec<store::TreeEntry>> = HashMap::new();
    let blob_paths = store::resolve_blob_paths(sql, &empty_pack_trees, &root_tree_hashes)?;

    // --- Process blobs ---
    for i in 0..index.len() {
        if types[i] != Some(pack::ObjectType::Blob) {
            continue;
        }
        let (_, data) = pack::resolve_entry(
            pack_data,
            &index,
            &offset_to_idx,
            i,
            &hash_to_idx,
            &mut cache,
        )
        .map_err(|e| Error::RustError(e.0))?;
        let hash = pack::hash_object(&pack::ObjectType::Blob, &data);
        hash_to_idx.insert(hash.clone(), i);
        let path = blob_paths
            .get(&hash)
            .map(|s| s.as_str())
            .unwrap_or("unknown");
        store::store_blob(sql, &hash, &data, path, KEYFRAME_INTERVAL)?;
    }

    // --- Handle REF_DELTA entries with unknown types ---
    for i in 0..index.len() {
        if types[i].is_some() {
            continue;
        }
        let resolved = pack::resolve_entry(
            pack_data,
            &index,
            &offset_to_idx,
            i,
            &hash_to_idx,
            &mut cache,
        );
        match resolved {
            Ok((obj_type, data)) => {
                let hash = pack::hash_object(&obj_type, &data);
                hash_to_idx.insert(hash.clone(), i);
                match obj_type {
                    pack::ObjectType::Commit => {
                        let parsed = store::parse_commit(&data)
                            .map_err(|e| Error::RustError(format!("commit {}: {}", hash, e)))?;
                        store::store_commit(sql, &hash, &parsed, &data)?;
                    }
                    pack::ObjectType::Tree => {
                        store::store_tree(sql, &hash, &data)?;
                    }
                    pack::ObjectType::Blob => {
                        let path = blob_paths
                            .get(&hash)
                            .map(|s| s.as_str())
                            .unwrap_or("unknown");
                        store::store_blob(sql, &hash, &data, path, KEYFRAME_INTERVAL)?;
                    }
                    pack::ObjectType::Tag => {}
                }
            }
            Err(_) => {}
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// git-upload-pack (handles `git clone` / `git fetch`)
// ---------------------------------------------------------------------------

/// Process a git-upload-pack POST request.
///
/// 1. Parse want/have lines from the client
/// 2. Walk the commit graph to collect all needed objects
/// 3. Reconstruct blob content from xpatch delta chains
/// 4. Generate and return a pack file
pub fn handle_upload_pack(sql: &SqlStorage, body: &[u8]) -> Result<Response> {
    // --- 1. Parse want/have negotiation ---
    let (wants, haves) = parse_want_have(body);

    if wants.is_empty() {
        return Err(Error::RustError("no want lines received".into()));
    }

    let have_set: HashSet<String> = haves.into_iter().collect();

    // --- 2-3. Collect all needed objects (commits, trees, blobs) ---
    let objects = store::collect_objects(sql, &wants, &have_set)?;

    // --- 4. Generate pack file ---
    let pack_data = pack::generate(&objects);

    // Build response: NAK + pack data
    let mut resp_body = Vec::new();
    pkt_line_bytes(&mut resp_body, b"NAK\n");
    resp_body.extend_from_slice(&pack_data);

    let mut resp = Response::from_bytes(resp_body)?;
    resp.headers_mut()
        .set("Content-Type", "application/x-git-upload-pack-result")?;
    Ok(resp)
}

/// Parse want/have lines from a git-upload-pack request body.
///
/// Format (pkt-line encoded):
///   want <hash>[ capabilities]\n
///   ...
///   [have <hash>\n]
///   ...
///   done\n
fn parse_want_have(data: &[u8]) -> (Vec<String>, Vec<String>) {
    let mut wants = Vec::new();
    let mut haves = Vec::new();
    let mut pos = 0;

    loop {
        match read_pkt_line(data, pos) {
            Some((None, new_pos)) => {
                // Flush packet — may separate wants from haves
                pos = new_pos;
            }
            Some((Some(line), new_pos)) => {
                pos = new_pos;
                let text = match std::str::from_utf8(line) {
                    Ok(t) => t.trim_end_matches('\n'),
                    Err(_) => continue,
                };

                if text == "done" {
                    break;
                } else if let Some(rest) = text.strip_prefix("want ") {
                    // First want line may have capabilities after a space
                    let hash = rest.split_whitespace().next().unwrap_or("");
                    if hash.len() == 40 {
                        wants.push(hash.to_string());
                    }
                } else if let Some(rest) = text.strip_prefix("have ") {
                    let hash = rest.split_whitespace().next().unwrap_or("");
                    if hash.len() == 40 {
                        haves.push(hash.to_string());
                    }
                }
            }
            None => break,
        }
    }

    (wants, haves)
}

// ---------------------------------------------------------------------------
// Pkt-line parsing for ref commands
// ---------------------------------------------------------------------------

struct RefCommand {
    old_hash: String,
    new_hash: String,
    ref_name: String,
}

/// Parse pkt-line encoded ref update commands from the start of the body.
/// Returns the commands and the byte offset where the pack data begins.
fn parse_ref_commands(data: &[u8]) -> (Vec<RefCommand>, usize) {
    let mut commands = Vec::new();
    let mut pos = 0;

    loop {
        match read_pkt_line(data, pos) {
            Some((None, new_pos)) => {
                // Flush packet: end of commands
                pos = new_pos;
                break;
            }
            Some((Some(line), new_pos)) => {
                pos = new_pos;
                if let Some(cmd) = parse_single_command(line) {
                    commands.push(cmd);
                }
            }
            None => break, // end of data
        }
    }

    (commands, pos)
}

/// Read one pkt-line from data at the given position.
/// Returns Some((None, new_pos)) for flush, Some((Some(payload), new_pos))
/// for data, or None if at end of input.
fn read_pkt_line(data: &[u8], pos: usize) -> Option<(Option<&[u8]>, usize)> {
    if pos + 4 > data.len() {
        return None;
    }

    let hex = std::str::from_utf8(&data[pos..pos + 4]).ok()?;
    let len = usize::from_str_radix(hex, 16).ok()?;

    if len == 0 {
        // Flush packet
        return Some((None, pos + 4));
    }
    if len < 4 || pos + len > data.len() {
        return None; // malformed
    }

    let payload = &data[pos + 4..pos + len];
    Some((Some(payload), pos + len))
}

/// Parse a single command line: "<old-hex> <new-hex> <refname>[\0capabilities]\n"
fn parse_single_command(line: &[u8]) -> Option<RefCommand> {
    // Strip trailing newline
    let line = if line.last() == Some(&b'\n') {
        &line[..line.len() - 1]
    } else {
        line
    };

    // Strip capabilities after NUL (first line only)
    let line = match line.iter().position(|&b| b == 0) {
        Some(pos) => &line[..pos],
        None => line,
    };

    let text = std::str::from_utf8(line).ok()?;
    let parts: Vec<&str> = text.splitn(3, ' ').collect();
    if parts.len() != 3 {
        return None;
    }

    Some(RefCommand {
        old_hash: parts[0].to_string(),
        new_hash: parts[1].to_string(),
        ref_name: parts[2].to_string(),
    })
}

// ---------------------------------------------------------------------------
// Report status
// ---------------------------------------------------------------------------

/// Build a report-status response in pkt-line format.
fn build_report_status(results: &[(String, std::result::Result<(), String>)]) -> Vec<u8> {
    let mut buf = Vec::new();

    pkt_line_bytes(&mut buf, b"unpack ok\n");

    for (ref_name, result) in results {
        match result {
            Ok(()) => {
                let line = format!("ok {}\n", ref_name);
                pkt_line_bytes(&mut buf, line.as_bytes());
            }
            Err(reason) => {
                let line = format!("ng {} {}\n", ref_name, reason);
                pkt_line_bytes(&mut buf, line.as_bytes());
            }
        }
    }

    buf.extend_from_slice(b"0000"); // flush
    buf
}

fn pkt_line_bytes(buf: &mut Vec<u8>, data: &[u8]) {
    let len = 4 + data.len();
    buf.extend_from_slice(format!("{:04x}", len).as_bytes());
    buf.extend_from_slice(data);
}
