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
    sql: SqlStorage,
    #[allow(dead_code)]
    env: Env,
}

impl DurableObject for Repository {
    fn new(state: State, env: Env) -> Self {
        let sql = state.storage().sql();
        schema::init(&sql);
        Self { sql, env }
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
