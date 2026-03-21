use super::*;

struct SettingsPage {
    owner: String,
    repo_name: String,
    commits: i64,
    blobs: i64,
    db_bytes: u64,
    default_branch: String,
}

impl SettingsPage {
    fn settings_path(&self) -> String {
        format!("/{}/{}/settings", self.owner, self.repo_name)
    }

    fn action_path(&self, sub: &str) -> String {
        format!("{}/{}", self.settings_path(), sub)
    }

    fn db_mb(&self) -> f64 {
        self.db_bytes as f64 / 1_048_576.0
    }
}

fn build_settings_page(sql: &SqlStorage, owner: &str, repo_name: &str) -> Result<SettingsPage> {
    #[derive(serde::Deserialize)]
    struct CountRow {
        n: i64,
    }

    let commits = sql
        .exec("SELECT COUNT(*) AS n FROM commits", None)?
        .to_array::<CountRow>()?
        .first()
        .map(|row| row.n)
        .unwrap_or(0);
    let blobs = sql
        .exec("SELECT COUNT(*) AS n FROM blobs", None)?
        .to_array::<CountRow>()?
        .first()
        .map(|row| row.n)
        .unwrap_or(0);

    Ok(SettingsPage {
        owner: owner.to_string(),
        repo_name: repo_name.to_string(),
        commits,
        blobs,
        db_bytes: sql.database_size() as u64,
        default_branch: store::get_config(sql, "default_branch")?
            .unwrap_or_else(|| "refs/heads/main".to_string()),
    })
}

fn render_settings_html(page: &SettingsPage, actor_name: Option<&str>) -> String {
    let mut html = String::new();
    html.push_str(&format!(
        r#"
<section class="settings-section">
  <h2>Repository stats</h2>
  <div class="stats-grid">
    <div class="stat-box"><div class="stat-val">{commits}</div><div class="stat-lbl">commits</div></div>
    <div class="stat-box"><div class="stat-val">{blobs}</div><div class="stat-lbl">blobs</div></div>
    <div class="stat-box"><div class="stat-val">{db_mb:.1} MB</div><div class="stat-lbl">database size</div></div>
  </div>
</section>"#,
        commits = page.commits,
        blobs = page.blobs,
        db_mb = page.db_mb(),
    ));

    html.push_str(&format!(
        r#"
<section class="settings-section">
  <h2>Search indexes</h2>
  <p class="settings-hint">Rebuild after a bulk push or if search results look stale.</p>
  <div class="action-row">
    <form method="POST" action="{commit_graph}">
      <button class="btn-action" type="submit">Rebuild commit graph</button>
      <span class="action-hint">Required for commit history and log</span>
    </form>
    <form method="POST" action="{fts_commits}">
      <button class="btn-action" type="submit">Rebuild commit search</button>
      <span class="action-hint">Full-text search over commit messages</span>
    </form>
    <form method="POST" action="{fts_head}">
      <button class="btn-action" type="submit">Rebuild code search</button>
      <span class="action-hint">Full-text search over file contents (slow on large repos)</span>
    </form>
  </div>
</section>"#,
        commit_graph = page.action_path("rebuild-graph"),
        fts_commits = page.action_path("rebuild-fts-commits"),
        fts_head = page.action_path("rebuild-fts"),
    ));

    html.push_str(&format!(
        r#"
<section class="settings-section">
  <h2>Default branch</h2>
  <form method="POST" action="{action}" class="inline-form">
    <input type="text" name="branch" value="{branch}" class="branch-input" placeholder="refs/heads/main">
    <button class="btn-action" type="submit">Save</button>
  </form>
</section>"#,
        action = page.action_path("default-branch"),
        branch = html_escape(&page.default_branch),
    ));

    html.push_str(&format!(
        r#"
<section class="settings-section settings-danger">
  <h2>Danger zone</h2>
  <p class="settings-hint">This will permanently delete all data for <strong>{owner}/{repo}</strong>. There is no undo.</p>
  <form method="POST" action="{action}" class="inline-form">
    <input type="text" name="confirm" placeholder='Type "{owner}/{repo}" to confirm' class="branch-input danger-confirm">
    <button class="btn-danger-action" type="submit">Delete repository</button>
  </form>
</section>"#,
        owner = html_escape(&page.owner),
        repo = html_escape(&page.repo_name),
        action = page.action_path("delete"),
    ));

    layout(
        "Settings",
        &page.owner,
        &page.repo_name,
        &page.default_branch,
        actor_name,
        &html,
    )
}

fn render_settings_markdown(page: &SettingsPage, selection: &NegotiatedRepresentation) -> String {
    let mut markdown = format!(
        "# {}/{} settings\n\nOwner-only repository maintenance page.\n\n## Repository Stats\n- Commits: `{}`\n- Blobs: `{}`\n- Database size: `{:.1} MB` (`{}` bytes)\n\n## Current Configuration\n- Settings page: `{}`\n- Default branch: `{}`\n- Code search rebuilds index the current default branch only.\n",
        page.owner,
        page.repo_name,
        page.commits,
        page.blobs,
        page.db_mb(),
        page.db_bytes,
        page.settings_path(),
        page.default_branch,
    );

    let actions = vec![
        Action::post(
            page.action_path("rebuild-graph"),
            "rebuild the commit ancestry graph used by history and log traversal",
        )
        .with_requires("repo owner")
        .with_effect("deletes existing `commit_graph` rows, regenerates them from `commit_parents`, then redirects back to settings"),
        Action::post(
            page.action_path("rebuild-fts-commits"),
            "rebuild the commit search index over commit messages and authors",
        )
        .with_requires("repo owner")
        .with_effect("clears `fts_commits`, re-inserts every commit, then redirects back to settings"),
        Action::post(
            page.action_path("rebuild-fts"),
            "rebuild the code search index for the saved default branch",
        )
        .with_requires("repo owner")
        .with_effect("looks up the current `default_branch`, rebuilds the HEAD file-content index from that ref when it exists, then redirects back to settings"),
        Action::post(
            page.action_path("default-branch"),
            "save the repository default branch used by the UI and code-search rebuilds",
        )
        .with_requires("repo owner")
        .with_fields(vec![presentation::ActionField::required(
            "branch",
            "full ref name to store, for example `refs/heads/main`; empty or whitespace-only input leaves the current value unchanged",
        )])
        .with_effect("stores `default_branch` exactly as submitted after trimming outer whitespace, then redirects back to settings"),
        Action::post(
            page.action_path("delete"),
            "permanently delete this repository",
        )
        .with_requires("repo owner")
        .with_fields(vec![presentation::ActionField::required(
            "confirm",
            &format!(
                "must exactly match `{}/{}` to proceed",
                page.owner, page.repo_name
            ),
        )])
        .with_effect(&format!(
            "danger: on an exact match, deletes all Durable Object storage for `{}/{}` and redirects to `/{}/`; any other value leaves the repository intact and redirects back to settings",
            page.owner, page.repo_name, page.owner
        )),
    ];

    let hints = vec![
        presentation::text_navigation_hint(*selection),
        Hint::new("All settings mutations here are POST-only and owner-only."),
        Hint::new("Use fully qualified refs like `refs/heads/main` for the default branch; this form does not verify that the ref exists before saving."),
        Hint::new("Danger: repository deletion is irreversible because it clears the repository Durable Object storage."),
    ];

    markdown.push_str(&presentation::render_actions_section(&actions));
    markdown.push_str(&presentation::render_hints_section(&hints));
    markdown
}

pub fn page_settings(
    sql: &SqlStorage,
    owner: &str,
    repo_name: &str,
    actor_name: Option<&str>,
) -> Result<Response> {
    let page = build_settings_page(sql, owner, repo_name)?;
    html_response(&render_settings_html(&page, actor_name))
}

pub fn page_settings_markdown(
    sql: &SqlStorage,
    owner: &str,
    repo_name: &str,
    selection: &NegotiatedRepresentation,
) -> Result<Response> {
    let page = build_settings_page(sql, owner, repo_name)?;
    presentation::markdown_response(&render_settings_markdown(&page, selection), selection)
}
