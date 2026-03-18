mod pack;
mod schema;

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

    async fn fetch(&self, req: Request) -> Result<Response> {
        let url = req.url()?;
        let path = url.path();
        let parts: Vec<&str> = path.trim_start_matches('/').split('/').collect();

        // Minimum path: /repo/:name/:something
        if parts.len() < 3 {
            return Response::error("Not Found", 404);
        }

        // Git smart HTTP protocol
        // GET  /repo/:name/info/refs?service=git-receive-pack
        // GET  /repo/:name/info/refs?service=git-upload-pack
        // POST /repo/:name/git-receive-pack
        // POST /repo/:name/git-upload-pack
        let action = parts[2];

        match (req.method(), action) {
            // -- Git protocol --
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
                // TODO: step 3
                Response::error("Not implemented", 501)
            }
            (Method::Post, "git-upload-pack") => {
                // TODO: step 4
                Response::error("Not implemented", 501)
            }

            // -- Read API --
            (Method::Get, "refs") => {
                // TODO: step 5
                Response::error("Not implemented", 501)
            }
            (Method::Get, "log") => {
                // TODO: step 5
                Response::error("Not implemented", 501)
            }
            (Method::Get, "tree") => {
                // TODO: step 5
                Response::error("Not implemented", 501)
            }
            (Method::Get, "blob") => {
                // TODO: step 5
                Response::error("Not implemented", 501)
            }
            (Method::Get, "file") => {
                // TODO: step 5
                Response::error("Not implemented", 501)
            }
            (Method::Get, "search") => {
                // TODO: step 6
                Response::error("Not implemented", 501)
            }
            (Method::Get, "stats") => {
                // TODO: step 5
                Response::error("Not implemented", 501)
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

        let caps = "report-status delete-refs ofs-delta side-band-64k";

        if refs.is_empty() {
            // Empty repo: advertise zero-id with capabilities
            let line = format!(
                "0000000000000000000000000000000000000000 capabilities^{{}}\0{}\n",
                caps
            );
            pkt_line(&mut body, &line);
        } else {
            for (i, r) in refs.iter().enumerate() {
                let line = if i == 0 {
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
