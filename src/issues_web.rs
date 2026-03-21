//! Server-rendered HTML pages for issues and pull requests.

use crate::{api, diff, issues, web};
use worker::*;

type Url = worker::Url;

// ---------------------------------------------------------------------------
// Issues list
// ---------------------------------------------------------------------------

pub fn page_issues_list(
    sql: &SqlStorage,
    owner: &str,
    repo_name: &str,
    url: &Url,
    actor_name: Option<&str>,
) -> Result<Response> {
    render_list(sql, owner, repo_name, url, actor_name, "issue")
}

pub fn page_pulls_list(
    sql: &SqlStorage,
    owner: &str,
    repo_name: &str,
    url: &Url,
    actor_name: Option<&str>,
) -> Result<Response> {
    render_list(sql, owner, repo_name, url, actor_name, "pr")
}

fn render_list(
    sql: &SqlStorage,
    owner: &str,
    repo_name: &str,
    url: &Url,
    actor_name: Option<&str>,
    kind: &str,
) -> Result<Response> {
    let (default_branch, _) = web::resolve_default_branch(sql)?;
    let state = api::get_query(url, "state").unwrap_or_else(|| "open".to_string());
    let offset: usize = api::get_query(url, "offset")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    const PAGE_SIZE: usize = 25;

    let items = issues::list_issues(sql, kind, &state, PAGE_SIZE + 1, offset)?;
    let has_more = items.len() > PAGE_SIZE;
    let items = &items[..items.len().min(PAGE_SIZE)];

    let open_count = issues::count_issues(sql, kind, "open")?;
    let closed_count = issues::count_issues_not_open(sql, kind)?;

    let (kind_label, kind_url, new_label, new_url) = if kind == "pr" {
        ("Pull Requests", "pulls", "New pull request", "pulls/new")
    } else {
        ("Issues", "issues", "New issue", "issues/new")
    };

    let is_open_tab = state == "open";
    let open_href = format!(
        "/{}/{}/{}",
        web::html_escape(owner),
        web::html_escape(repo_name),
        kind_url
    );
    let closed_href = format!(
        "/{}/{}/{}?state=closed",
        web::html_escape(owner),
        web::html_escape(repo_name),
        kind_url
    );

    let new_href = format!(
        "/{}/{}/{}",
        web::html_escape(owner),
        web::html_escape(repo_name),
        new_url
    );

    // Tab bar
    let tabs = format!(
        r#"<div class="issue-tabs">
          <a href="{open_href}" class="issue-tab{open_active}">{open_count} Open</a>
          <a href="{closed_href}" class="issue-tab{closed_active}">{closed_count} Closed</a>
        </div>"#,
        open_href = open_href,
        open_active = if is_open_tab { " active" } else { "" },
        open_count = open_count,
        closed_href = closed_href,
        closed_active = if !is_open_tab { " active" } else { "" },
        closed_count = closed_count,
    );

    // New button (authenticated only)
    let new_btn = if actor_name.is_some() {
        format!(
            r#"<a href="{}" class="btn-primary">{}</a>"#,
            new_href, new_label
        )
    } else {
        String::new()
    };

    // Items
    let mut items_html = String::new();
    if items.is_empty() {
        items_html.push_str(r#"<div class="issue-empty">No items found.</div>"#);
    } else {
        for item in items {
            let state_class = match item.state.as_str() {
                "merged" => "merged",
                "closed" => "closed",
                _ => "open",
            };
            let state_icon = match item.state.as_str() {
                "merged" => "⟳",
                "closed" => "✓",
                _ => "●",
            };
            let detail_href = format!(
                "/{}/{}/{}/{}",
                web::html_escape(owner),
                web::html_escape(repo_name),
                kind_url,
                item.number
            );
            let branch_info = if kind == "pr" {
                match (&item.source_branch, &item.target_branch) {
                    (Some(src), Some(tgt)) => format!(
                        r#" <span class="pr-branch-pair"><code>{}</code> → <code>{}</code></span>"#,
                        web::html_escape(src),
                        web::html_escape(tgt)
                    ),
                    _ => String::new(),
                }
            } else {
                String::new()
            };
            items_html.push_str(&format!(
                r#"<div class="issue-item">
                  <span class="issue-state-icon {state_class}">{icon}</span>
                  <div class="issue-item-main">
                    <a href="{href}" class="issue-item-title">{title}</a>{branch_info}
                    <div class="issue-item-meta">
                      #{num} opened {time} by <strong>{author}</strong>
                    </div>
                  </div>
                </div>"#,
                state_class = state_class,
                icon = state_icon,
                href = detail_href,
                title = web::html_escape(&item.title),
                branch_info = branch_info,
                num = item.number,
                time = web::format_time(item.created_at),
                author = web::html_escape(&item.author_name),
            ));
        }
    }

    // Pagination
    let mut pagination = String::new();
    if offset > 0 {
        let prev_offset = if offset >= PAGE_SIZE {
            offset - PAGE_SIZE
        } else {
            0
        };
        pagination.push_str(&format!(
            r#"<a href="/{}/{}/{}?state={}&offset={}" class="btn-action">← Newer</a>"#,
            web::html_escape(owner),
            web::html_escape(repo_name),
            kind_url,
            state,
            prev_offset
        ));
    }
    if has_more {
        pagination.push_str(&format!(
            r#"<a href="/{}/{}/{}?state={}&offset={}" class="btn-action">Older →</a>"#,
            web::html_escape(owner),
            web::html_escape(repo_name),
            kind_url,
            state,
            offset + PAGE_SIZE
        ));
    }
    if !pagination.is_empty() {
        pagination = format!(r#"<div class="pagination">{}</div>"#, pagination);
    }

    let content = format!(
        r#"<div class="issue-list-header">
          <h1>{kind_label}</h1>
          {new_btn}
        </div>
        {tabs}
        <div class="issue-list">{items_html}</div>
        {pagination}"#,
        kind_label = kind_label,
        new_btn = new_btn,
        tabs = tabs,
        items_html = items_html,
        pagination = pagination,
    );

    web::html_response(&web::layout(
        kind_label,
        owner,
        repo_name,
        &default_branch,
        actor_name,
        &content,
    ))
}

// ---------------------------------------------------------------------------
// Issue / PR detail
// ---------------------------------------------------------------------------

pub fn page_issue_detail(
    sql: &SqlStorage,
    owner: &str,
    repo_name: &str,
    number: i64,
    actor_name: Option<&str>,
) -> Result<Response> {
    let (default_branch, _) = web::resolve_default_branch(sql)?;

    let issue = match issues::get_issue(sql, number)? {
        Some(i) => i,
        None => return Response::error("Not Found", 404),
    };

    let comments = issues::list_comments(sql, issue.id)?;

    let is_pr = issue.kind == "pr";
    let kind_url = if is_pr { "pulls" } else { "issues" };

    let state_class = match issue.state.as_str() {
        "merged" => "merged",
        "closed" => "closed",
        _ => "open",
    };
    let state_label = match issue.state.as_str() {
        "merged" => "Merged",
        "closed" => "Closed",
        _ => "Open",
    };

    // PR diff section
    let pr_diff_section = if is_pr {
        render_pr_diff_section(sql, owner, repo_name, &issue, actor_name)?
    } else {
        String::new()
    };

    // Comments HTML
    let mut comments_html = String::new();
    for c in &comments {
        comments_html.push_str(&render_comment(
            &c.author_name,
            c.created_at,
            &c.body,
            false,
            "",
        ));
    }

    // Comment form (authenticated only)
    let comment_form = if actor_name.is_some() {
        format!(
            r#"<div class="comment comment-form-wrap">
              <div class="comment-header-row">
                <strong>{actor}</strong>
              </div>
              <form method="POST" action="/{owner}/{repo}/{kind_url}/{num}/comment" class="comment-form">
                <textarea name="body" class="comment-textarea" placeholder="Leave a comment..." rows="5"></textarea>
                <div class="comment-form-footer">
                  {state_btn}
                  <button type="submit" class="btn-primary">Comment</button>
                </div>
              </form>
            </div>"#,
            actor = web::html_escape(actor_name.unwrap_or("")),
            owner = web::html_escape(owner),
            repo = web::html_escape(repo_name),
            kind_url = kind_url,
            num = number,
            state_btn = render_state_button(&issue, owner, repo_name, kind_url, actor_name),
        )
    } else {
        String::new()
    };

    // Title and meta
    let branch_meta = if is_pr {
        match (&issue.source_branch, &issue.target_branch) {
            (Some(src), Some(tgt)) => format!(
                r#"<div class="pr-branch-info">
                  <code class="branch-tag">{}</code>
                  <span class="arrow">→</span>
                  <code class="branch-tag">{}</code>
                </div>"#,
                web::html_escape(src),
                web::html_escape(tgt),
            ),
            _ => String::new(),
        }
    } else {
        String::new()
    };

    let page_title = if is_pr { "Pull Request" } else { "Issue" };

    let content = format!(
        r#"<div class="issue-detail-header">
          <div class="issue-title-row">
            <h1>{title} <span class="issue-number-heading">#{num}</span></h1>
          </div>
          <div class="issue-meta-row">
            <span class="issue-badge {state_class}">{state_label}</span>
            {branch_meta}
            <span class="issue-meta-text">
              opened {time} by <strong>{author}</strong>
            </span>
          </div>
        </div>

        <div class="issue-body-wrap">
          {body_html}
        </div>

        {pr_diff_section}

        <div class="comment-thread">
          {comments_html}
        </div>

        {comment_form}"#,
        title = web::html_escape(&issue.title),
        num = number,
        state_class = state_class,
        state_label = state_label,
        branch_meta = branch_meta,
        time = web::format_time(issue.created_at),
        author = web::html_escape(&issue.author_name),
        body_html = render_issue_body(&issue.body),
        pr_diff_section = pr_diff_section,
        comments_html = comments_html,
        comment_form = comment_form,
    );

    web::html_response(&web::layout(
        &format!("{} #{}", page_title, number),
        owner,
        repo_name,
        &default_branch,
        actor_name,
        &content,
    ))
}

fn render_issue_body(body: &str) -> String {
    if body.trim().is_empty() {
        r#"<p class="issue-no-description">No description provided.</p>"#.to_string()
    } else {
        format!(
            r#"<div class="issue-body readme-body">{}</div>"#,
            web::render_markdown(body)
        )
    }
}

fn render_comment(
    author: &str,
    created_at: i64,
    body: &str,
    is_first: bool,
    _extra_class: &str,
) -> String {
    let _ = is_first;
    format!(
        r#"<div class="comment">
          <div class="comment-header">
            <strong>{author}</strong>
            <span class="comment-time">{time}</span>
          </div>
          <div class="comment-body readme-body">{body}</div>
        </div>"#,
        author = web::html_escape(author),
        time = web::format_time(created_at),
        body = web::render_markdown(body),
    )
}

fn render_state_button(
    issue: &issues::IssueRow,
    owner: &str,
    repo_name: &str,
    kind_url: &str,
    actor_name: Option<&str>,
) -> String {
    let actor = match actor_name {
        Some(a) => a,
        None => return String::new(),
    };
    if issue.state == "merged" {
        return String::new(); // cannot reopen a merged PR
    }
    // author or owner can close/reopen
    if actor != issue.author_name && actor != owner {
        return String::new();
    }
    if issue.state == "open" {
        format!(
            r#"<button type="submit"
                formaction="/{owner}/{repo}/{kind}/{num}/close"
                class="btn-action">Close</button>"#,
            owner = web::html_escape(owner),
            repo = web::html_escape(repo_name),
            kind = kind_url,
            num = issue.number,
        )
    } else {
        format!(
            r#"<button type="submit"
                formaction="/{owner}/{repo}/{kind}/{num}/reopen"
                class="btn-action">Reopen</button>"#,
            owner = web::html_escape(owner),
            repo = web::html_escape(repo_name),
            kind = kind_url,
            num = issue.number,
        )
    }
}

fn render_pr_diff_section(
    sql: &SqlStorage,
    owner: &str,
    repo_name: &str,
    issue: &issues::IssueRow,
    actor_name: Option<&str>,
) -> Result<String> {
    let source_branch = match &issue.source_branch {
        Some(b) => b.as_str(),
        None => return Ok(String::new()),
    };
    let target_branch = match &issue.target_branch {
        Some(b) => b.as_str(),
        None => return Ok(String::new()),
    };

    // For merged PRs, show the merge commit's diff (frozen at merge time).
    // For open/closed PRs, show merge-base → source (what the PR introduces).
    let (files, stats) = if issue.state == "merged" {
        if let Some(ref merge_hash) = issue.merge_commit_hash {
            let d = diff::diff_commit(sql, merge_hash, true, 3)?;
            (d.files, d.stats)
        } else {
            return Ok(String::new());
        }
    } else {
        let source_ref = format!("refs/heads/{}", source_branch);
        let target_ref = format!("refs/heads/{}", target_branch);

        let source_hash = match api::resolve_ref(sql, &source_ref)? {
            Some(h) => h,
            None => {
                return Ok(format!(
                    r#"<div class="pr-diff-section">
                      <p class="pr-branch-missing">Branch <code>{}</code> no longer exists.</p>
                    </div>"#,
                    web::html_escape(source_branch)
                ))
            }
        };
        let target_hash = match api::resolve_ref(sql, &target_ref)? {
            Some(h) => h,
            None => {
                return Ok(format!(
                    r#"<div class="pr-diff-section">
                      <p class="pr-branch-missing">Target branch <code>{}</code> not found.</p>
                    </div>"#,
                    web::html_escape(target_branch)
                ))
            }
        };

        let base_hash = issues::find_merge_base(sql, &source_hash, &target_hash)?
            .unwrap_or_else(|| target_hash.clone());
        let c = diff::compare(sql, &base_hash, &source_hash, true, 3)?;
        (c.files, c.stats)
    };

    let stats_html = format!(
        r#"<div class="pr-diff-stats">
          <span>{} file{} changed</span>
          <span class="stat-add">+{}</span>
          <span class="stat-del">-{}</span>
        </div>"#,
        stats.files_changed,
        if stats.files_changed == 1 { "" } else { "s" },
        stats.additions,
        stats.deletions,
    );

    let mut files_html = String::new();
    for file in &files {
        files_html.push_str(&crate::web::render_file_diff(file));
    }

    // Merge button (owner only, PR must be open)
    let merge_section = if issue.state == "open" && actor_name == Some(owner) {
        format!(
            r#"<div class="pr-merge-box">
              <form method="POST"
                    action="/{owner}/{repo}/pulls/{num}/merge">
                <button type="submit" class="btn-merge">Merge pull request</button>
              </form>
              <p class="pr-merge-hint">
                Creates a merge commit on <code>{target}</code>.
                Conflicts return a 409 error.
              </p>
            </div>"#,
            owner = web::html_escape(owner),
            repo = web::html_escape(repo_name),
            num = issue.number,
            target = web::html_escape(target_branch),
        )
    } else if issue.state == "merged" {
        let merge_hash = issue.merge_commit_hash.as_deref().unwrap_or("");
        format!(
            r#"<div class="pr-merged-box">
              PR merged in <a href="/{owner}/{repo}/commit/{hash}">{short}</a>.
            </div>"#,
            owner = web::html_escape(owner),
            repo = web::html_escape(repo_name),
            hash = web::html_escape(merge_hash),
            short = web::html_escape(&merge_hash[..merge_hash.len().min(7)]),
        )
    } else {
        String::new()
    };

    Ok(format!(
        r#"<div class="pr-diff-section">
          <h2>Files changed</h2>
          {stats_html}
          {merge_section}
          {files_html}
        </div>"#,
        stats_html = stats_html,
        merge_section = merge_section,
        files_html = files_html,
    ))
}

// ---------------------------------------------------------------------------
// New issue form
// ---------------------------------------------------------------------------

pub fn page_new_issue(
    sql: &SqlStorage,
    owner: &str,
    repo_name: &str,
    actor_name: Option<&str>,
) -> Result<Response> {
    let (default_branch, _) = web::resolve_default_branch(sql)?;

    let content = format!(
        r#"<h1>New Issue</h1>
        <form method="POST" action="/{owner}/{repo}/issues" class="new-issue-form">
          <div class="form-group">
            <label class="form-label" for="title">Title</label>
            <input type="text" id="title" name="title" class="form-input"
                   placeholder="Issue title" required autofocus>
          </div>
          <div class="form-group">
            <label class="form-label" for="body">Description <span class="form-hint">(Markdown)</span></label>
            <textarea id="body" name="body" class="form-textarea"
                      placeholder="Describe the issue..." rows="12"></textarea>
          </div>
          <div class="form-actions">
            <button type="submit" class="btn-primary">Submit new issue</button>
            <a href="/{owner}/{repo}/issues" class="btn-action">Cancel</a>
          </div>
        </form>"#,
        owner = web::html_escape(owner),
        repo = web::html_escape(repo_name),
    );

    web::html_response(&web::layout(
        "New Issue",
        owner,
        repo_name,
        &default_branch,
        actor_name,
        &content,
    ))
}

// ---------------------------------------------------------------------------
// New pull request form
// ---------------------------------------------------------------------------

pub fn page_new_pull(
    sql: &SqlStorage,
    owner: &str,
    repo_name: &str,
    url: &Url,
    actor_name: Option<&str>,
) -> Result<Response> {
    let (default_branch, _) = web::resolve_default_branch(sql)?;

    // Load available branches
    let branches = load_branches(sql)?;
    if branches.len() < 2 {
        let content = r#"<p>You need at least two branches to open a pull request.</p>"#;
        return web::html_response(&web::layout(
            "New Pull Request",
            owner,
            repo_name,
            &default_branch,
            actor_name,
            content,
        ));
    }

    let preselect_source = api::get_query(url, "source").unwrap_or_default();
    let preselect_target = api::get_query(url, "target").unwrap_or_else(|| default_branch.clone());

    let source_options: String = branches
        .iter()
        .map(|b| {
            let sel = if b == &preselect_source {
                " selected"
            } else {
                ""
            };
            format!(
                r#"<option value="{v}"{sel}>{v}</option>"#,
                v = web::html_escape(b),
                sel = sel
            )
        })
        .collect();

    let target_options: String = branches
        .iter()
        .map(|b| {
            let sel = if b == &preselect_target {
                " selected"
            } else {
                ""
            };
            format!(
                r#"<option value="{v}"{sel}>{v}</option>"#,
                v = web::html_escape(b),
                sel = sel
            )
        })
        .collect();

    let content = format!(
        r#"<h1>New Pull Request</h1>
        <form method="POST" action="/{owner}/{repo}/pulls" class="new-issue-form">
          <div class="form-group form-branch-row">
            <div>
              <label class="form-label">Base branch <span class="form-hint">(merge into)</span></label>
              <select name="target" class="branch-input">{target_options}</select>
            </div>
            <span class="arrow">←</span>
            <div>
              <label class="form-label">Compare branch <span class="form-hint">(changes from)</span></label>
              <select name="source" class="branch-input">{source_options}</select>
            </div>
          </div>
          <div class="form-group">
            <label class="form-label" for="pr-title">Title</label>
            <input type="text" id="pr-title" name="title" class="form-input"
                   placeholder="Pull request title" required autofocus>
          </div>
          <div class="form-group">
            <label class="form-label" for="pr-body">Description <span class="form-hint">(Markdown)</span></label>
            <textarea id="pr-body" name="body" class="form-textarea"
                      placeholder="Describe your changes..." rows="10"></textarea>
          </div>
          <div class="form-actions">
            <button type="submit" class="btn-primary">Open pull request</button>
            <a href="/{owner}/{repo}/pulls" class="btn-action">Cancel</a>
          </div>
        </form>"#,
        owner = web::html_escape(owner),
        repo = web::html_escape(repo_name),
        source_options = source_options,
        target_options = target_options,
    );

    web::html_response(&web::layout(
        "New Pull Request",
        owner,
        repo_name,
        &default_branch,
        actor_name,
        &content,
    ))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn load_branches(sql: &SqlStorage) -> Result<Vec<String>> {
    #[derive(serde::Deserialize)]
    struct Row {
        name: String,
    }
    let rows: Vec<Row> = sql
        .exec(
            "SELECT name FROM refs WHERE name LIKE 'refs/heads/%' ORDER BY name",
            None,
        )?
        .to_array()?;
    Ok(rows
        .into_iter()
        .map(|r| {
            r.name
                .strip_prefix("refs/heads/")
                .unwrap_or(&r.name)
                .to_string()
        })
        .collect())
}
