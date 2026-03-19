mod api;
mod diff;
mod git;
mod pack;
mod schema;
mod store;
mod web;

use worker::*;

/// Delta compression keyframe interval. A full keyframe is stored every N
/// versions within a blob group. Worst-case reconstruction applies N-1 deltas.
pub const KEYFRAME_INTERVAL: i64 = 50;

// ---------------------------------------------------------------------------
// Worker entry point — route to the named Repository DO
// ---------------------------------------------------------------------------

#[event(fetch)]
async fn fetch(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let url = req.url()?;
    let path = url.path();
    let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();

    // All paths under /repo/:name are handled by a DO instance named after
    // the repository. This includes both git smart HTTP endpoints and the
    // read API.
    if parts.len() >= 2 && parts[0] == "repo" {
        let repo_name = parts[1];
        let namespace = env.durable_object("REPOSITORY")?;
        let id = namespace.id_from_name(repo_name)?;
        let stub = id.get_stub()?;
        return stub.fetch_with_request(req).await;
    }

    Response::from_json(&serde_json::json!({
        "name": "ripgit",
        "version": "0.1.0",
        "description": "Git remote backed by Cloudflare Durable Objects"
    }))
}

// ---------------------------------------------------------------------------
// Repository Durable Object
// ---------------------------------------------------------------------------

#[durable_object]
pub struct Repository {
    state: State,
    sql: SqlStorage,
    #[allow(dead_code)]
    env: Env,
}

impl DurableObject for Repository {
    fn new(state: State, env: Env) -> Self {
        let state = state;
        let sql = state.storage().sql();
        schema::init(&sql);
        Self { sql, env, state }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        let url = req.url()?;
        let path = url.path();
        let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();

        // Minimum: ["repo", ":name"]
        if parts.len() < 2 {
            return Response::error("Not Found", 404);
        }

        let repo_name = parts[1];
        let action = if parts.len() >= 3 { parts[2] } else { "" };

        // Does the client want HTML? (browsers send Accept: text/html)
        let wants_html = req
            .headers()
            .get("Accept")
            .ok()
            .flatten()
            .map(|a| a.contains("text/html"))
            .unwrap_or(false);

        match (req.method(), action) {
            // -- Git smart HTTP protocol --
            (Method::Get, "info") if parts.get(3) == Some(&"refs") => {
                let service = url
                    .query_pairs()
                    .find(|(k, _)| k == "service")
                    .map(|(_, v)| v.to_string())
                    .unwrap_or_default();
                match service.as_str() {
                    "git-receive-pack" => self.advertise_refs("git-receive-pack"),
                    "git-upload-pack" => self.advertise_refs("git-upload-pack"),
                    _ => Response::error("Unsupported service", 403),
                }
            }
            (Method::Post, "git-receive-pack") => {
                let body = req.bytes().await?;
                git::handle_receive_pack(&self.sql, &body)
            }
            (Method::Post, "git-upload-pack") => {
                let body = req.bytes().await?;
                git::handle_upload_pack(&self.sql, &body)
            }

            // -- Delete all data (for testing) --
            (Method::Delete, "") => {
                self.state.storage().delete_all().await?;
                Response::ok("deleted")
            }

            // -- JSON API (always JSON) --
            (Method::Get, "refs") => api::handle_refs(&self.sql),
            (Method::Get, "file") => api::handle_file(&self.sql, &url),
            (Method::Get, "search") => api::handle_search(&self.sql, &url),
            (Method::Get, "stats") => api::handle_stats(&self.sql),

            // -- Diff API (always JSON) --
            (Method::Get, "diff") if !wants_html => {
                let sha = parts.get(3).unwrap_or(&"");
                diff::handle_diff(&self.sql, sha, &url)
            }
            (Method::Get, "compare") => {
                let spec = parts.get(3).unwrap_or(&"");
                diff::handle_compare(&self.sql, spec, &url)
            }

            // -- Content-negotiated: JSON for API, HTML for browsers --
            (Method::Get, "log") if !wants_html => api::handle_log(&self.sql, &url),
            (Method::Get, "commit") if !wants_html => {
                let hash = parts.get(3).unwrap_or(&"");
                api::handle_commit(&self.sql, hash)
            }
            // tree/blob by 40-hex hash → JSON API
            (Method::Get, "tree") if is_hex40(parts.get(3).unwrap_or(&"")) => {
                api::handle_tree(&self.sql, parts.get(3).unwrap_or(&""))
            }
            (Method::Get, "blob") if is_hex40(parts.get(3).unwrap_or(&"")) => {
                api::handle_blob(&self.sql, parts.get(3).unwrap_or(&""))
            }

            // -- Web UI --
            (Method::Get, "") => web::page_home(&self.sql, repo_name, &url),
            (Method::Get, "log") => web::page_log(&self.sql, repo_name, &url),
            (Method::Get, "commit") | (Method::Get, "diff") => {
                let hash = parts.get(3).unwrap_or(&"");
                web::page_commit(&self.sql, repo_name, hash)
            }
            (Method::Get, "tree") => {
                let ref_name = parts.get(3).unwrap_or(&"main");
                let sub_path = if parts.len() > 4 {
                    parts[4..].join("/")
                } else {
                    String::new()
                };
                web::page_tree(&self.sql, repo_name, ref_name, &sub_path)
            }
            (Method::Get, "blob") => {
                let ref_name = parts.get(3).unwrap_or(&"main");
                let sub_path = if parts.len() > 4 {
                    parts[4..].join("/")
                } else {
                    String::new()
                };
                web::page_blob(&self.sql, repo_name, ref_name, &sub_path)
            }
            (Method::Get, "search-ui") => web::page_search(&self.sql, repo_name, &url),

            // -- Admin endpoints --
            (Method::Put, "admin") => {
                let sub = parts.get(3).unwrap_or(&"");
                match *sub {
                    "set-ref" => {
                        let name = url.query_pairs().find(|(k, _)| k == "name").map(|(_, v)| v.to_string());
                        let hash = url.query_pairs().find(|(k, _)| k == "hash").map(|(_, v)| v.to_string());
                        match (name, hash) {
                            (Some(n), Some(h)) => {
                                self.sql.exec(
                                    "INSERT INTO refs (name, commit_hash) VALUES (?, ?)
                                     ON CONFLICT(name) DO UPDATE SET commit_hash = ?",
                                    vec![
                                        SqlStorageValue::from(n.clone()),
                                        SqlStorageValue::from(h.clone()),
                                        SqlStorageValue::from(h.clone()),
                                    ],
                                )?;
                                Response::ok(format!("{} -> {}", n, h))
                            }
                            _ => Response::ok("need ?name=refs/heads/main&hash=abc123"),
                        }
                    }
                    "config" => {
                        let key = url.query_pairs().find(|(k, _)| k == "key").map(|(_, v)| v.to_string());
                        let value = url.query_pairs().find(|(k, _)| k == "value").map(|(_, v)| v.to_string());
                        match (key, value) {
                            (Some(k), Some(v)) => {
                                store::set_config(&self.sql, &k, &v)?;
                                Response::ok(format!("{} = {}", k, v))
                            }
                            (Some(k), None) => {
                                let v = store::get_config(&self.sql, &k)?;
                                Response::ok(v.unwrap_or_else(|| "(not set)".to_string()))
                            }
                            _ => Response::ok("need ?key=name[&value=val]"),
                        }
                    }
                    "rebuild-fts" => {
                        let default_ref = store::get_config(&self.sql, "default_branch")?
                            .unwrap_or_else(|| "refs/heads/main".to_string());
                        #[derive(serde::Deserialize)]
                        struct RefRow { commit_hash: String }
                        let rows: Vec<RefRow> = self.sql.exec(
                            "SELECT commit_hash FROM refs WHERE name = ?",
                            vec![SqlStorageValue::from(default_ref)],
                        )?.to_array()?;
                        if let Some(row) = rows.first() {
                            store::rebuild_fts_index(&self.sql, &row.commit_hash)?;
                            Response::ok("fts rebuilt")
                        } else {
                            Response::ok("no default branch ref found")
                        }
                    }
                    "rebuild-graph" => {
                        // Bulk rebuild commit graph using INSERT...SELECT per level.
                        // ~14 SQL calls for any repo size.
                        self.sql.exec("DELETE FROM commit_graph", None)?;

                        // Level 0: direct first-parent
                        self.sql.exec(
                            "INSERT INTO commit_graph (commit_hash, level, ancestor_hash)
                             SELECT cp.commit_hash, 0, cp.parent_hash
                             FROM commit_parents cp WHERE cp.ordinal = 0",
                            None,
                        )?;

                        let mut level: i64 = 1;
                        loop {
                            let prev = level - 1;
                            let result = self.sql.exec(
                                &format!(
                                    "INSERT INTO commit_graph (commit_hash, level, ancestor_hash)
                                     SELECT cg.commit_hash, {}, cg2.ancestor_hash
                                     FROM commit_graph cg
                                     JOIN commit_graph cg2
                                       ON cg2.commit_hash = cg.ancestor_hash AND cg2.level = {}
                                     WHERE cg.level = {}",
                                    level, prev, prev
                                ),
                                None,
                            )?;
                            if result.rows_written() == 0 {
                                break;
                            }
                            level += 1;
                        }

                        Response::ok(format!("commit graph rebuilt ({} levels)", level))
                    }
                    "rebuild-fts-commits" => {
                        // Bulk rebuild fts_commits from all commits
                        self.sql.exec("DELETE FROM fts_commits", None)?;
                        self.sql.exec(
                            "INSERT INTO fts_commits (hash, message, author)
                             SELECT hash, message, author FROM commits",
                            None,
                        )?;
                        #[derive(serde::Deserialize)]
                        struct Count { n: i64 }
                        let rows: Vec<Count> = self.sql.exec(
                            "SELECT COUNT(*) AS n FROM fts_commits", None
                        )?.to_array()?;
                        let n = rows.first().map(|r| r.n).unwrap_or(0);
                        Response::ok(format!("fts_commits rebuilt ({} entries)", n))
                    }
                    _ => Response::error("unknown admin action", 404),
                }
            }

            _ => Response::error("Not Found", 404),
        }
    }
}

// ---------------------------------------------------------------------------
// Git protocol helpers
// ---------------------------------------------------------------------------

impl Repository {
    /// Ref advertisement for both receive-pack and upload-pack.
    /// Returns current refs in pkt-line format so git knows what we have.
    fn advertise_refs(&self, service: &str) -> Result<Response> {
        let content_type = format!("application/x-{}-advertisement", service);

        // Collect current refs
        #[derive(serde::Deserialize)]
        struct RefRow {
            name: String,
            commit_hash: String,
        }
        let refs: Vec<RefRow> = self
            .sql
            .exec("SELECT name, commit_hash FROM refs", None)?
            .to_array()?;

        let mut body = Vec::new();

        // Service announcement
        let svc_line = format!("# service={}\n", service);
        pkt_line(&mut body, &svc_line);
        body.extend_from_slice(b"0000"); // flush

        // Build capabilities, including symref for HEAD
        let default_branch = store::get_config(&self.sql, "default_branch")?
            .unwrap_or_else(|| "refs/heads/main".to_string());
        let caps = format!(
            "report-status delete-refs ofs-delta symref=HEAD:{}",
            default_branch
        );

        if refs.is_empty() {
            // Empty repo: advertise zero-id with capabilities
            let line = format!(
                "0000000000000000000000000000000000000000 capabilities^{{}}\0{}\n",
                caps
            );
            pkt_line(&mut body, &line);
        } else {
            // Find the default branch's commit for HEAD
            let head_hash = refs
                .iter()
                .find(|r| r.name == default_branch)
                .map(|r| r.commit_hash.clone());

            let mut first = true;

            // Advertise HEAD first (so git clone checks out the right branch)
            if let Some(ref hh) = head_hash {
                let line = format!("{} HEAD\0{}\n", hh, caps);
                pkt_line(&mut body, &line);
                first = false;
            }

            for r in refs.iter() {
                let line = if first {
                    first = false;
                    format!("{} {}\0{}\n", r.commit_hash, r.name, caps)
                } else {
                    format!("{} {}\n", r.commit_hash, r.name)
                };
                pkt_line(&mut body, &line);
            }
        }
        body.extend_from_slice(b"0000"); // flush

        let mut resp = Response::from_bytes(body)?;
        resp.headers_mut()
            .set("Content-Type", &content_type)?;
        resp.headers_mut()
            .set("Cache-Control", "no-cache")?;
        Ok(resp)
    }
}

// ---------------------------------------------------------------------------
// Pkt-line encoding
// ---------------------------------------------------------------------------

/// Append a pkt-line encoded string to the buffer.
/// Pkt-line format: 4 hex digits for total length (including the 4 digits),
/// followed by the payload.
fn pkt_line(buf: &mut Vec<u8>, data: &str) {
    let len = 4 + data.len();
    buf.extend_from_slice(format!("{:04x}", len).as_bytes());
    buf.extend_from_slice(data.as_bytes());
}

/// Check if a string is a 40-character hex SHA-1 hash.
/// Used to distinguish API calls (by hash) from web UI calls (by ref + path).
fn is_hex40(s: &str) -> bool {
    s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit())
}
