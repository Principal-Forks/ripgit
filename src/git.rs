//! Git smart HTTP protocol handlers for receive-pack and upload-pack.

use crate::{pack, store, KEYFRAME_INTERVAL};
use std::collections::{HashMap, HashSet};
use worker::*;

// ---------------------------------------------------------------------------
// git-receive-pack (handles `git push`)
// ---------------------------------------------------------------------------

/// Process a git-receive-pack POST request.
///
/// 1. Parse pkt-line ref update commands
/// 2. Parse the pack file
/// 3. Store all objects (commits, trees, blobs with xpatch delta compression)
/// 4. Update refs
/// 5. Return report-status
pub fn handle_receive_pack(sql: &SqlStorage, body: &[u8]) -> Result<Response> {
    // --- 1. Parse ref update commands from pkt-lines ---
    let (commands, pack_offset) = parse_ref_commands(body);

    if commands.is_empty() {
        return Err(Error::RustError("no ref update commands received".into()));
    }

    // --- 2. Parse pack file (if present) ---
    let pack_data = &body[pack_offset..];
    let objects = if pack_data.len() > 4 && &pack_data[..4] == b"PACK" {
        pack::parse(pack_data).map_err(|e| Error::RustError(e.0))?
    } else {
        Vec::new()
    };

    // --- 3. Store all objects ---

    // Build lookup maps for trees (needed for blob path resolution)
    let mut pack_trees: HashMap<String, Vec<store::TreeEntry>> = HashMap::new();
    let mut root_tree_hashes: Vec<String> = Vec::new();

    // First pass: store commits and trees, collect metadata
    for obj in &objects {
        match obj.obj_type {
            pack::ObjectType::Commit => {
                let parsed = store::parse_commit(&obj.data)
                    .map_err(|e| Error::RustError(format!("commit {}: {}", obj.hash, e)))?;
                root_tree_hashes.push(parsed.tree_hash.clone());
                store::store_commit(sql, &obj.hash, &parsed, &obj.data)?;
            }
            pack::ObjectType::Tree => {
                let entries = store::parse_tree_data(&obj.data)
                    .map_err(|e| Error::RustError(format!("tree {}: {}", obj.hash, e)))?;
                pack_trees.insert(obj.hash.clone(), entries);
                store::store_tree(sql, &obj.hash, &obj.data)?;
            }
            _ => {} // blobs and tags handled below
        }
    }

    // Resolve blob paths by walking commit trees
    let blob_paths = store::resolve_blob_paths(sql, &pack_trees, &root_tree_hashes)?;

    // Second pass: store blobs with delta compression
    for obj in &objects {
        match obj.obj_type {
            pack::ObjectType::Blob => {
                let path = blob_paths
                    .get(&obj.hash)
                    .map(|s| s.as_str())
                    .unwrap_or("unknown");
                store::store_blob(sql, &obj.hash, &obj.data, path, KEYFRAME_INTERVAL)?;
            }
            pack::ObjectType::Tag => {
                // Annotated tags: store as a commit-like object.
                // For now, we skip storing tag objects and just handle
                // lightweight tags via refs.
            }
            _ => {} // already handled
        }
    }

    // --- 4. Update refs ---
    let mut results: Vec<(String, std::result::Result<(), String>)> = Vec::new();

    for cmd in &commands {
        let result = store::update_ref(sql, &cmd.ref_name, &cmd.old_hash, &cmd.new_hash)
            .map_err(|e| format!("{}", e));
        results.push((cmd.ref_name.clone(), result));
    }

    // --- 5. Return report-status ---
    let status_body = build_report_status(&results);

    let mut resp = Response::from_bytes(status_body)?;
    resp.headers_mut()
        .set("Content-Type", "application/x-git-receive-pack-result")?;
    Ok(resp)
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
