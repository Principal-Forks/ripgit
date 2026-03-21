//! Web UI: server-rendered HTML pages for browsing repositories.
//!
//! All HTML is generated from Rust using `format!()`. No build step,
//! no static assets, no framework. Highlight.js loaded from CDN for
//! syntax highlighting in the file viewer.

use crate::{api, diff, store};
use worker::*;

type Url = worker::Url;

// ---------------------------------------------------------------------------
// Layout: shared HTML shell
// ---------------------------------------------------------------------------

pub(crate) fn layout(
    title: &str,
    owner: &str,
    repo_name: &str,
    default_branch: &str,
    actor_name: Option<&str>,
    content: &str,
) -> String {
    let is_owner = actor_name == Some(owner);
    let global_auth = match actor_name {
        Some(name) => format!(
            r#"<a href="/{n}" class="nav-user">{n}</a><a href="/logout" class="nav-signout">Sign out</a>"#,
            n = html_escape(name),
        ),
        None => format!(
            r#"<a href="/login?next=/{o}/{r}/" class="nav-signin">Sign in</a>"#,
            o = html_escape(owner),
            r = html_escape(repo_name),
        ),
    };
    let repo_settings_link = if is_owner {
        format!(
            r#"<a href="/{}/{}/settings">Settings</a>"#,
            html_escape(owner),
            html_escape(repo_name)
        )
    } else {
        String::new()
    };
    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{title} - {owner}/{repo_name} - ripgit</title>
<link rel="stylesheet" href="https://cdnjs.cloudflare.com/ajax/libs/highlight.js/11.9.0/styles/github.min.css">
<style>
{CSS}
</style>
</head>
<body>
<header>
  <div class="global-nav">
    <a href="/" class="logo">ripgit</a>
    <div class="global-auth">{global_auth}</div>
  </div>
  <div class="repo-bar">
    <div class="repo-crumb">
      <a href="/{owner}/" class="owner-name">{owner}</a>
      <span class="sep">/</span>
      <a href="/{owner}/{repo_name}/" class="repo-name">{repo_name}</a>
    </div>
    <div class="repo-search">
      <input type="text" id="nav-search-input" placeholder="Search..." autocomplete="off" spellcheck="false">
      <div id="nav-search-results" class="nav-search-results" hidden></div>
    </div>
    <nav class="repo-tabs">
      <a href="/{owner}/{repo_name}/">Code</a>
      <a href="/{owner}/{repo_name}/commits">Commits</a>
      <a href="/{owner}/{repo_name}/issues">Issues</a>
      <a href="/{owner}/{repo_name}/pulls">PRs</a>
      {repo_settings_link}
    </nav>
  </div>
</header>
<main>
{content}
</main>
<script src="https://cdnjs.cloudflare.com/ajax/libs/highlight.js/11.9.0/highlight.min.js"></script>
<script src="https://cdnjs.cloudflare.com/ajax/libs/highlightjs-line-numbers.js/2.8.0/highlightjs-line-numbers.min.js"></script>
<script>
hljs.highlightAll();
hljs.initLineNumbersOnLoad({{singleLine:true}});
setTimeout(function(){{
  var m=window.location.hash.match(/^#L(\d+)$/);
  if(m){{var td=document.querySelector('td[data-line-number="'+m[1]+'"]');
    if(td){{var tr=td.parentNode;tr.style.background='#fffbdd';tr.scrollIntoView({{block:'center'}})}}
  }}
}},150);
(function(){{
  var inp=document.getElementById('nav-search-input');
  var box=document.getElementById('nav-search-results');
  if(!inp||!box) return;
  var timer=null;
  var repoPath='{owner}/{repo_name}';
  var branch='{default_branch}';
  function esc(s){{return String(s).replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;');}}
  function scopeOf(q){{return /@(author|message):/.test(q)?'commits':'code';}}
  inp.addEventListener('input',function(){{
    clearTimeout(timer);
    var q=inp.value.trim();
    if(!q){{box.hidden=true;box.innerHTML='';return;}}
    timer=setTimeout(function(){{doSearch(q);}},200);
  }});
  inp.addEventListener('keydown',function(e){{
    if(e.key==='Enter'){{
      var q=inp.value.trim();
      if(q) window.location.href='/'+repoPath+'/search-ui?q='+encodeURIComponent(q)+'&scope='+scopeOf(q);
      e.preventDefault();
    }}else if(e.key==='Escape'){{
      box.hidden=true;inp.blur();
    }}
  }});
  document.addEventListener('click',function(e){{
    if(!inp.closest('.repo-search').contains(e.target)) box.hidden=true;
  }});
  function doSearch(q){{
    fetch('/'+repoPath+'/search?q='+encodeURIComponent(q)+'&scope='+scopeOf(q)+'&limit=10')
      .then(function(r){{return r.json();}})
      .then(function(data){{
        if(inp.value.trim()!==q) return;
        if(!data.results||!data.results.length){{
          box.innerHTML='<div class="nsr-empty">No results</div>';
          box.hidden=false;return;
        }}
        var html='';
        if(data.scope==='commits'){{
          data.results.forEach(function(r){{
            html+='<a class="nsr-item" href="/'+repoPath+'/commit/'+esc(r.hash)+'">'
              +'<div class="nsr-path">'+esc(r.hash.slice(0,7))+' \u2014 '+esc((r.message||'').split('\n')[0].slice(0,72))+'</div>'
              +'<div class="nsr-snippet">'+esc(r.author)+'</div>'
              +'</a>';
          }});
          html+='<a class="nsr-all" href="/'+repoPath+'/search-ui?q='+encodeURIComponent(q)+'&scope=commits">'
            +'View all \u2014 '+data.results.length+' commit'+(data.results.length===1?'':'s')
            +'</a>';
        }}else{{
          data.results.forEach(function(r){{
            var m=r.matches[0];
            var snippet=m?m.text.replace(/^\s+/,''):'';
            html+='<a class="nsr-item" href="/'+repoPath+'/blob/'+esc(branch)+'/'+esc(r.path)+(m?'#L'+m.line:'')+'">'
              +'<div class="nsr-path">'+esc(r.path)+'</div>'
              +(snippet?'<div class="nsr-snippet">'+esc(snippet)+'</div>':'')
              +'</a>';
          }});
          html+='<a class="nsr-all" href="/'+repoPath+'/search-ui?q='+encodeURIComponent(q)+'&scope=code">'
            +'View all \u2014 '+data.total_matches+' match'+(data.total_matches===1?'':'es')+' in '+data.total_files+' file'+(data.total_files===1?'':'s')
            +'</a>';
        }}
        box.innerHTML=html;box.hidden=false;
      }})
      .catch(function(){{}});
  }}
}})();
</script>
</body>
</html>"#,
        title = html_escape(title),
        owner = html_escape(owner),
        repo_name = html_escape(repo_name),
        default_branch = html_escape(default_branch),
        global_auth = global_auth,
        repo_settings_link = repo_settings_link,
        content = content,
        CSS = CSS,
    )
}

const CSS: &str = r#"
* { margin: 0; padding: 0; box-sizing: border-box; }
body {
  font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Helvetica, Arial, sans-serif;
  font-size: 14px;
  color: #1f2328;
  background: #fff;
  line-height: 1.5;
}
a { color: #0969da; text-decoration: none; }
a:hover { text-decoration: underline; }
/* ── Global nav (identity only) ─────────────────────────────── */
.global-nav {
  display: flex;
  align-items: center;
  justify-content: space-between;
  max-width: 1200px;
  margin: 0 auto;
  padding: 0 24px;
  height: 44px;
}
header { border-bottom: 1px solid #d1d9e0; }
.logo { font-weight: 700; font-size: 15px; color: #1f2328; }
.global-auth { display: flex; align-items: center; gap: 10px; font-size: 13px; }

/* ── Repo bar (context + actions) ───────────────────────────── */
.repo-bar {
  display: flex;
  align-items: center;
  gap: 16px;
  max-width: 1200px;
  margin: 0 auto;
  padding: 0 24px;
  height: 40px;
  border-top: 1px solid #d1d9e0;
  background: #f6f8fa;
}
.repo-crumb { display: flex; align-items: center; gap: 4px; flex-shrink: 0; }
.repo-search { flex: 0 0 auto; }
.repo-tabs { margin-left: auto; display: flex; align-items: center; gap: 16px; flex-shrink: 0; }
.repo-tabs a { font-size: 13px; color: #656d76; }
.repo-tabs a:hover { color: #1f2328; text-decoration: none; }
.sep { color: #656d76; }
.owner-name { font-weight: 600; color: #1f2328; font-size: 13px; }
.repo-name { font-weight: 600; color: #1f2328; font-size: 13px; }
main {
  max-width: 1200px;
  margin: 0 auto;
  padding: 24px;
}
h1 { font-size: 20px; margin-bottom: 16px; }
h2 { font-size: 16px; margin-bottom: 12px; }

/* File tree */
.tree-table { width: 100%; border-collapse: collapse; }
.tree-table td {
  padding: 6px 12px;
  border-top: 1px solid #d1d9e0;
}
.tree-table tr:first-child td { border-top: none; }
.tree-icon { width: 20px; color: #656d76; }
.tree-name { }
.tree-msg { color: #656d76; text-align: right; }

/* Commit list */
.commit-list { list-style: none; }
.commit-item {
  padding: 8px 0;
  border-bottom: 1px solid #d1d9e0;
  display: flex;
  align-items: baseline;
  gap: 12px;
}
.commit-msg { flex: 1; }
.commit-msg a { color: #1f2328; }
.commit-hash {
  font-family: ui-monospace, SFMono-Regular, monospace;
  font-size: 12px;
  color: #0969da;
  background: #ddf4ff;
  padding: 2px 6px;
  border-radius: 4px;
}
.commit-time { color: #656d76; font-size: 12px; white-space: nowrap; }
.commit-author { color: #656d76; font-size: 12px; }

/* File viewer */
.file-header {
  display: flex;
  align-items: center;
  justify-content: space-between;
  padding: 8px 12px;
  background: #f6f8fa;
  border: 1px solid #d1d9e0;
  border-radius: 6px 6px 0 0;
}
.raw-btn {
  font-size: 12px;
  padding: 2px 8px;
  border: 1px solid #d1d9e0;
  border-radius: 4px;
  color: #1f2328;
  background: #fff;
  text-decoration: none;
}
.raw-btn:hover {
  background: #f6f8fa;
  text-decoration: none;
}
.nav-signin { color: #1f2328; border: 1px solid #d1d9e0; border-radius: 4px; padding: 3px 10px; text-decoration: none; background: #fff; }
.nav-signin:hover { background: #f6f8fa; }
.nav-user { font-weight: 600; color: #1f2328; text-decoration: none; }
.nav-signout { color: #656d76; text-decoration: none; }
.empty-repo { padding: 24px; background: #f6f8fa; border: 1px solid #d1d9e0; border-radius: 6px; }
.empty-repo h2 { margin-bottom: 12px; }
.empty-repo-msg { color: #656d76; }
.push-cmd { background: #fff; border: 1px solid #d1d9e0; border-radius: 6px; padding: 14px 16px; font-size: 13px; margin: 12px 0; overflow-x: auto; line-height: 1.6; }
.push-note { font-size: 13px; color: #656d76; margin-top: 8px; }
.push-note a { color: #0969da; }
.settings-section { border: 1px solid #d1d9e0; border-radius: 6px; padding: 20px 24px; margin-bottom: 20px; }
.settings-section h2 { font-size: 16px; margin-bottom: 10px; }
.settings-hint { font-size: 13px; color: #656d76; margin-bottom: 14px; }
.settings-danger { border-color: #cf222e; }
.settings-danger h2 { color: #cf222e; }
.stats-grid { display: flex; gap: 16px; flex-wrap: wrap; }
.stat-box { background: #f6f8fa; border: 1px solid #d1d9e0; border-radius: 6px; padding: 12px 20px; min-width: 100px; text-align: center; }
.stat-val { font-size: 22px; font-weight: 600; }
.stat-lbl { font-size: 12px; color: #656d76; margin-top: 2px; }
.action-row { display: flex; flex-direction: column; gap: 10px; }
.action-row form { display: flex; align-items: center; gap: 12px; }
.action-hint { font-size: 13px; color: #656d76; }
.btn-action { background: #f6f8fa; border: 1px solid #d1d9e0; border-radius: 6px; padding: 5px 14px; font-size: 13px; cursor: pointer; }
.btn-action:hover { background: #eaeef2; }
.btn-danger-action { background: #cf222e; color: #fff; border: none; border-radius: 6px; padding: 5px 14px; font-size: 13px; cursor: pointer; }
.btn-danger-action:hover { background: #a40e26; }
.inline-form { display: flex; gap: 8px; align-items: center; }
.branch-input { border: 1px solid #d1d9e0; border-radius: 6px; padding: 5px 10px; font-size: 13px; font-family: ui-monospace, monospace; width: 280px; }
.danger-confirm { width: 320px; }
.branch-input:focus { outline: none; border-color: #0969da; }
.file-content {
  border: 1px solid #d1d9e0;
  border-top: none;
  border-radius: 0 0 6px 6px;
  overflow-x: auto;
}
.file-content pre {
  margin: 0;
  padding: 12px;
  font-size: 13px;
  line-height: 1.45;
}
.file-content code { font-family: ui-monospace, SFMono-Regular, monospace; }
.line-numbers {
  display: inline-block;
  text-align: right;
  padding-right: 12px;
  margin-right: 12px;
  border-right: 1px solid #d1d9e0;
  color: #656d76;
  user-select: none;
  min-width: 40px;
}

/* Diff */
.diff-file {
  margin-bottom: 16px;
  border: 1px solid #d1d9e0;
  border-radius: 6px;
  overflow: hidden;
}
.diff-file-header {
  padding: 8px 12px;
  background: #f6f8fa;
  border-bottom: 1px solid #d1d9e0;
  font-family: ui-monospace, SFMono-Regular, monospace;
  font-size: 13px;
  display: flex;
  align-items: center;
  gap: 8px;
}
.diff-status {
  font-size: 11px;
  font-weight: 600;
  padding: 1px 6px;
  border-radius: 3px;
  text-transform: uppercase;
}
.diff-status.added { background: #dafbe1; color: #116329; }
.diff-status.deleted { background: #ffebe9; color: #82071e; }
.diff-status.modified { background: #ddf4ff; color: #0550ae; }
.diff-hunk-header {
  padding: 4px 12px;
  background: #ddf4ff;
  color: #656d76;
  font-family: ui-monospace, SFMono-Regular, monospace;
  font-size: 12px;
  border-top: 1px solid #d1d9e0;
}
.diff-table { width: 100%; border-collapse: collapse; font-size: 13px; }
.diff-table td {
  padding: 0 12px;
  font-family: ui-monospace, SFMono-Regular, monospace;
  white-space: pre-wrap;
  word-break: break-all;
  vertical-align: top;
  line-height: 20px;
}
.diff-ln {
  width: 1%;
  min-width: 40px;
  text-align: right;
  color: #656d76;
  user-select: none;
  padding: 0 8px;
}
.diff-line-add { background: #dafbe1; }
.diff-line-add .diff-ln { background: #ccffd8; }
.diff-line-del { background: #ffebe9; }
.diff-line-del .diff-ln { background: #ffd7d5; }
.diff-line-ctx { background: #fff; }

/* Stats */
.diff-stats {
  margin-bottom: 16px;
  padding: 12px;
  background: #f6f8fa;
  border: 1px solid #d1d9e0;
  border-radius: 6px;
  font-size: 13px;
}
.stat-add { color: #116329; font-weight: 600; }
.stat-del { color: #82071e; font-weight: 600; }

/* Search */
.search-form {
  display: flex;
  gap: 8px;
  margin-bottom: 16px;
}
.search-form input[type="text"] {
  flex: 1;
  padding: 8px 12px;
  border: 1px solid #d1d9e0;
  border-radius: 6px;
  font-size: 14px;
}
.search-form button {
  padding: 8px 16px;
  background: #2da44e;
  color: #fff;
  border: none;
  border-radius: 6px;
  cursor: pointer;
  font-size: 14px;
}
.search-result {
  padding: 12px;
  border: 1px solid #d1d9e0;
  border-radius: 6px;
  margin-bottom: 8px;
}
.search-result-path {
  font-family: ui-monospace, SFMono-Regular, monospace;
  font-size: 13px;
  margin-bottom: 4px;
}
.search-result-snippet {
  font-size: 13px;
  color: #656d76;
  white-space: pre-wrap;
}

/* Breadcrumb */
.breadcrumb {
  margin-bottom: 16px;
  font-size: 16px;
}
.breadcrumb a { font-weight: 600; }

/* README */
.readme-box {
  margin-top: 24px;
  border: 1px solid #d1d9e0;
  border-radius: 6px;
}
.readme-header {
  padding: 8px 12px;
  background: #f6f8fa;
  border-bottom: 1px solid #d1d9e0;
  font-weight: 600;
  font-size: 13px;
}
.readme-body {
  padding: 16px 24px;
  font-size: 14px;
  line-height: 1.6;
}
.readme-body pre {
  background: #f6f8fa;
  padding: 12px;
  border-radius: 6px;
  overflow-x: auto;
}

/* Pagination */
.pagination {
  display: flex;
  gap: 8px;
  margin-top: 16px;
}
.pagination a, .pagination span {
  padding: 6px 12px;
  border: 1px solid #d1d9e0;
  border-radius: 6px;
  font-size: 13px;
}
.pagination span { color: #656d76; }

/* Branch selector */
.branch-selector {
  display: inline-flex;
  align-items: center;
  gap: 8px;
  margin-bottom: 16px;
}
.branch-selector select {
  padding: 5px 8px;
  border: 1px solid #d1d9e0;
  border-radius: 6px;
  background: #f6f8fa;
  font-size: 13px;
  font-weight: 600;
  cursor: pointer;
}
.branch-selector .branch-label {
  font-size: 13px;
  color: #656d76;
}

/* Line numbers (highlightjs-line-numbers.js overrides) */
.hljs-ln-numbers {
  text-align: right;
  padding-right: 12px !important;
  border-right: 1px solid #d1d9e0;
  color: #656d76;
  user-select: none;
  min-width: 40px;
  vertical-align: top;
}
.hljs-ln-code {
  padding-left: 12px !important;
}

/* Markdown */
.readme-body h1 { font-size: 24px; margin: 16px 0 8px; padding-bottom: 4px; border-bottom: 1px solid #d1d9e0; }
.readme-body h2 { font-size: 20px; margin: 14px 0 6px; padding-bottom: 4px; border-bottom: 1px solid #d1d9e0; }
.readme-body h3 { font-size: 16px; margin: 12px 0 4px; }
.readme-body p { margin: 8px 0; }
.readme-body ul, .readme-body ol { margin: 8px 0; padding-left: 24px; }
.readme-body li { margin: 2px 0; }
.readme-body code {
  font-family: ui-monospace, SFMono-Regular, monospace;
  font-size: 13px;
  background: #eff1f3;
  padding: 2px 6px;
  border-radius: 4px;
}
.readme-body pre code { background: none; padding: 0; }
.readme-body a { color: #0969da; }
.readme-body hr { border: none; border-top: 1px solid #d1d9e0; margin: 16px 0; }
.readme-body blockquote {
  border-left: 3px solid #d1d9e0;
  padding-left: 12px;
  color: #656d76;
  margin: 8px 0;
}

/* Nav search */
.repo-search {
  position: relative;
}
.repo-search input {
  padding: 4px 10px;
  border: 1px solid #d1d9e0;
  border-radius: 6px;
  font-size: 13px;
  width: 220px;
  background: #fff;
  color: #1f2328;
}
.repo-search input:focus {
  outline: none;
  border-color: #0969da;
  box-shadow: 0 0 0 3px rgba(9,105,218,0.1);
}
/* Keep .nav-search as an alias so the JS selector still works */
.nav-search { position: relative; }
.nav-search-results {
  position: absolute;
  top: calc(100% + 6px);
  right: 0;
  width: 460px;
  background: #fff;
  border: 1px solid #d1d9e0;
  border-radius: 8px;
  box-shadow: 0 8px 24px rgba(0,0,0,0.12);
  z-index: 100;
  overflow: hidden;
  max-height: 460px;
  overflow-y: auto;
}
.nsr-item {
  display: block;
  padding: 8px 12px;
  border-bottom: 1px solid #f0f2f4;
  text-decoration: none;
  color: #1f2328;
}
.nsr-item:hover { background: #f6f8fa; text-decoration: none; }
.nsr-path {
  font-family: ui-monospace, SFMono-Regular, monospace;
  font-size: 12px;
  color: #0969da;
}
.nsr-snippet {
  font-family: ui-monospace, SFMono-Regular, monospace;
  font-size: 12px;
  color: #656d76;
  white-space: nowrap;
  overflow: hidden;
  text-overflow: ellipsis;
  margin-top: 2px;
}
.nsr-empty {
  padding: 16px 12px;
  color: #656d76;
  font-size: 13px;
  text-align: center;
}
.nsr-all {
  display: block;
  padding: 8px 12px;
  font-size: 13px;
  color: #0969da;
  text-align: center;
  border-top: 1px solid #d1d9e0;
  background: #f6f8fa;
  text-decoration: none;
}
.nsr-all:hover { background: #eef1f5; text-decoration: none; }

/* ── Issues & PRs ───────────────────────────────────────────── */
.issue-list-header { display: flex; align-items: center; justify-content: space-between; margin-bottom: 12px; }
.issue-tabs { display: flex; gap: 0; border: 1px solid #d1d9e0; border-radius: 6px; overflow: hidden; margin-bottom: 0; }
.issue-tab { padding: 6px 16px; font-size: 13px; color: #656d76; background: #f6f8fa; border-right: 1px solid #d1d9e0; text-decoration: none; }
.issue-tab:last-child { border-right: none; }
.issue-tab.active { background: #fff; color: #1f2328; font-weight: 600; }
.issue-tab:hover { background: #eaeef2; text-decoration: none; }
.issue-list { border: 1px solid #d1d9e0; border-radius: 6px; overflow: hidden; margin-top: 8px; }
.issue-item { display: flex; align-items: flex-start; gap: 10px; padding: 10px 16px; border-bottom: 1px solid #d1d9e0; }
.issue-item:last-child { border-bottom: none; }
.issue-item:hover { background: #f6f8fa; }
.issue-state-icon { font-size: 14px; margin-top: 2px; flex-shrink: 0; }
.issue-state-icon.open { color: #1a7f37; }
.issue-state-icon.closed { color: #656d76; }
.issue-state-icon.merged { color: #8250df; }
.issue-item-main { flex: 1; min-width: 0; }
.issue-item-title { font-weight: 600; color: #1f2328; word-break: break-word; }
.issue-item-title:hover { color: #0969da; text-decoration: none; }
.issue-item-meta { font-size: 12px; color: #656d76; margin-top: 2px; }
.pr-branch-pair { font-size: 12px; color: #656d76; margin-left: 8px; }
.pr-branch-pair code { font-size: 12px; background: #f6f8fa; padding: 1px 5px; border-radius: 3px; border: 1px solid #d1d9e0; }
.issue-empty { padding: 24px; color: #656d76; text-align: center; }
.issue-badge { display: inline-flex; align-items: center; gap: 4px; padding: 3px 10px; border-radius: 20px; font-size: 12px; font-weight: 600; }
.issue-badge.open { background: #dafbe1; color: #1a7f37; }
.issue-badge.closed { background: #f0f2f4; color: #656d76; }
.issue-badge.merged { background: #eee4ff; color: #8250df; }
.issue-detail-header { margin-bottom: 20px; padding-bottom: 16px; border-bottom: 1px solid #d1d9e0; }
.issue-title-row h1 { font-size: 22px; margin-bottom: 8px; }
.issue-number-heading { color: #656d76; font-weight: 400; }
.issue-meta-row { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
.issue-meta-text { font-size: 13px; color: #656d76; }
.issue-body-wrap { border: 1px solid #d1d9e0; border-radius: 6px; padding: 16px 20px; margin-bottom: 20px; }
.issue-body { line-height: 1.6; }
.issue-no-description { color: #656d76; font-style: italic; font-size: 14px; }
.comment { border: 1px solid #d1d9e0; border-radius: 6px; margin-bottom: 12px; overflow: hidden; }
.comment-header { display: flex; align-items: center; gap: 8px; padding: 8px 14px; background: #f6f8fa; border-bottom: 1px solid #d1d9e0; font-size: 13px; }
.comment-time { color: #656d76; font-size: 12px; }
.comment-body { padding: 12px 16px; font-size: 14px; line-height: 1.6; }
.comment-form-wrap { border: 1px solid #d1d9e0; border-radius: 6px; margin-top: 16px; overflow: hidden; }
.comment-header-row { padding: 8px 14px; background: #f6f8fa; border-bottom: 1px solid #d1d9e0; font-size: 13px; font-weight: 600; }
.comment-form { padding: 12px 16px; }
.comment-textarea { width: 100%; padding: 8px 12px; border: 1px solid #d1d9e0; border-radius: 6px; font-size: 14px; font-family: inherit; resize: vertical; }
.comment-textarea:focus { outline: none; border-color: #0969da; box-shadow: 0 0 0 3px rgba(9,105,218,0.1); }
.comment-form-footer { display: flex; justify-content: flex-end; gap: 8px; margin-top: 8px; }
.comment-thread { margin: 16px 0; }
.pr-diff-section { margin: 20px 0; }
.pr-diff-section h2 { font-size: 16px; margin-bottom: 10px; }
.pr-diff-stats { display: flex; gap: 12px; align-items: center; padding: 8px 14px; background: #f6f8fa; border: 1px solid #d1d9e0; border-radius: 6px; font-size: 13px; margin-bottom: 12px; }
.pr-merge-box { padding: 16px; background: #dafbe1; border: 1px solid #aef0be; border-radius: 6px; margin-bottom: 12px; display: flex; align-items: center; gap: 12px; flex-wrap: wrap; }
.pr-merged-box { padding: 12px 16px; background: #eee4ff; border: 1px solid #d8b4fe; border-radius: 6px; margin-bottom: 12px; font-size: 13px; }
.pr-merge-hint { font-size: 12px; color: #1a7f37; margin: 0; }
.pr-branch-info { display: flex; align-items: center; gap: 8px; }
.branch-tag { background: #f6f8fa; border: 1px solid #d1d9e0; padding: 2px 8px; border-radius: 4px; font-size: 13px; }
.pr-branch-missing { color: #cf222e; font-size: 14px; }
.btn-primary { padding: 6px 16px; background: #2da44e; color: #fff; border: none; border-radius: 6px; font-size: 13px; cursor: pointer; text-decoration: none; display: inline-block; }
.btn-primary:hover { background: #2c974b; color: #fff; text-decoration: none; }
.btn-merge { padding: 8px 20px; background: #8250df; color: #fff; border: none; border-radius: 6px; font-size: 14px; cursor: pointer; font-weight: 600; }
.btn-merge:hover { background: #6e40c9; }
.new-issue-form { max-width: 760px; }
.form-group { margin-bottom: 16px; }
.form-label { display: block; font-weight: 600; font-size: 13px; margin-bottom: 6px; }
.form-hint { font-weight: 400; color: #656d76; }
.form-input { width: 100%; padding: 8px 12px; border: 1px solid #d1d9e0; border-radius: 6px; font-size: 14px; font-family: inherit; }
.form-input:focus { outline: none; border-color: #0969da; box-shadow: 0 0 0 3px rgba(9,105,218,0.1); }
.form-textarea { width: 100%; padding: 8px 12px; border: 1px solid #d1d9e0; border-radius: 6px; font-size: 14px; font-family: inherit; resize: vertical; }
.form-textarea:focus { outline: none; border-color: #0969da; box-shadow: 0 0 0 3px rgba(9,105,218,0.1); }
.form-actions { display: flex; gap: 8px; align-items: center; }
.form-branch-row { display: flex; align-items: flex-end; gap: 12px; flex-wrap: wrap; }
.form-branch-row .arrow { font-size: 18px; color: #656d76; padding-bottom: 6px; }
"#;

// ---------------------------------------------------------------------------
// Page: Repository home
// ---------------------------------------------------------------------------

pub fn page_home(
    sql: &SqlStorage,
    owner: &str,
    repo_name: &str,
    url: &Url,
    actor_name: Option<&str>,
) -> Result<Response> {
    // Use ?ref= param if provided, otherwise detect default branch
    let (ref_name, head_hash) = if let Some(r) = api::get_query(url, "ref") {
        let hash = api::resolve_ref(sql, &r)?;
        (r, hash)
    } else {
        resolve_default_branch(sql)?
    };

    let content = match head_hash {
        Some(hash) => {
            let tree_hash = load_tree_for_commit(sql, &hash)?;
            let entries = load_sorted_tree(sql, &tree_hash)?;

            let mut html = String::new();

            // Branch selector
            html.push_str(&render_branch_selector(
                sql, owner, repo_name, &ref_name, "home", "",
            )?);

            // File tree
            html.push_str(r#"<table class="tree-table">"#);
            for e in &entries {
                let icon = if e.is_tree { "&#128193;" } else { "&#128196;" };
                let link = if e.is_tree {
                    format!("/{}/{}/tree/{}/{}", owner, repo_name, ref_name, e.name)
                } else {
                    format!("/{}/{}/blob/{}/{}", owner, repo_name, ref_name, e.name)
                };
                html.push_str(&format!(
                    r#"<tr><td class="tree-icon">{icon}</td><td class="tree-name"><a href="{link}">{name}</a></td></tr>"#,
                    icon = icon,
                    link = link,
                    name = html_escape(&e.name),
                ));
            }
            html.push_str("</table>");

            // Recent commits
            html.push_str(r#"<h2 style="margin-top:24px">Recent commits</h2>"#);
            html.push_str(&render_commit_list(sql, &hash, 5, owner, repo_name)?);

            // README
            html.push_str(&render_readme(
                sql, &tree_hash, owner, repo_name, &ref_name,
            )?);

            html
        }
        None => {
            if actor_name == Some(owner) {
                // Show push instructions for the repo owner
                let host = url.host_str().unwrap_or("your-worker.dev");
                let scheme = url.scheme();
                format!(
                    r#"<div class="empty-repo">
<h2>This repository is empty</h2>
<p>Push your first commit to get started:</p>
<pre class="push-cmd">cd my-project
git init
git add .
git commit -m "initial commit"
git remote add origin {scheme}://{owner}:TOKEN@{host}/{owner}/{repo_name}
git push origin main</pre>
<p class="push-note">Replace <code>TOKEN</code> with an access token from <a href="/settings">Settings</a>.</p>
</div>"#,
                    scheme = scheme,
                    owner = owner,
                    repo_name = repo_name,
                    host = host,
                )
            } else {
                "<p class=\"empty-repo-msg\">Empty repository.</p>".to_string()
            }
        }
    };

    html_response(&layout(
        "Home", owner, repo_name, &ref_name, actor_name, &content,
    ))
}

// ---------------------------------------------------------------------------
// Page: Tree browser
// ---------------------------------------------------------------------------

pub fn page_tree(
    sql: &SqlStorage,
    owner: &str,
    repo_name: &str,
    ref_name: &str,
    path: &str,
    actor_name: Option<&str>,
) -> Result<Response> {
    let commit_hash = match api::resolve_ref(sql, ref_name)? {
        Some(h) => h,
        None => return Response::error("ref not found", 404),
    };

    let tree_hash = resolve_path_to_tree(sql, &commit_hash, path)?;
    let entries = load_sorted_tree(sql, &tree_hash)?;

    let mut html = String::new();

    // Branch selector
    html.push_str(&render_branch_selector(
        sql, owner, repo_name, ref_name, "tree", path,
    )?);

    // Breadcrumb
    html.push_str(&render_breadcrumb(owner, repo_name, ref_name, path, true));

    // Tree table
    html.push_str(r#"<table class="tree-table">"#);
    // Parent directory link
    if !path.is_empty() {
        let parent = parent_path(path);
        let parent_link = if parent.is_empty() {
            format!("/{}/{}/", owner, repo_name)
        } else {
            format!("/{}/{}/tree/{}/{}", owner, repo_name, ref_name, parent)
        };
        html.push_str(&format!(
            r#"<tr><td class="tree-icon">&#128193;</td><td class="tree-name"><a href="{}">..</a></td></tr>"#,
            parent_link
        ));
    }

    for e in &entries {
        let icon = if e.is_tree { "&#128193;" } else { "&#128196;" };
        let full = if path.is_empty() {
            e.name.clone()
        } else {
            format!("{}/{}", path, e.name)
        };
        let link = if e.is_tree {
            format!("/{}/{}/tree/{}/{}", owner, repo_name, ref_name, full)
        } else {
            format!("/{}/{}/blob/{}/{}", owner, repo_name, ref_name, full)
        };
        html.push_str(&format!(
            r#"<tr><td class="tree-icon">{icon}</td><td class="tree-name"><a href="{link}">{name}</a></td></tr>"#,
            icon = icon,
            link = link,
            name = html_escape(&e.name),
        ));
    }
    html.push_str("</table>");

    html_response(&layout(path, owner, repo_name, ref_name, actor_name, &html))
}

// ---------------------------------------------------------------------------
// Page: Blob viewer
// ---------------------------------------------------------------------------

pub fn page_blob(
    sql: &SqlStorage,
    owner: &str,
    repo_name: &str,
    ref_name: &str,
    path: &str,
    actor_name: Option<&str>,
) -> Result<Response> {
    let commit_hash = match api::resolve_ref(sql, ref_name)? {
        Some(h) => h,
        None => return Response::error("ref not found", 404),
    };

    let blob_hash = resolve_path_to_blob(sql, &commit_hash, path)?;
    let content = load_blob(sql, &blob_hash)?;

    let mut html = String::new();

    // Branch selector
    html.push_str(&render_branch_selector(
        sql, owner, repo_name, ref_name, "blob", path,
    )?);

    // Breadcrumb
    html.push_str(&render_breadcrumb(owner, repo_name, ref_name, path, false));

    // File header
    let filename = path.rsplit('/').next().unwrap_or(path);
    let size = content.len();
    html.push_str(&format!(
        r#"<div class="file-header"><span>{filename}</span><div style="display:flex;gap:8px;align-items:center"><span style="color:#656d76;font-size:13px">{size} bytes</span><a href="/{owner}/{repo}/raw/{ref_name}/{path}" class="raw-btn">Raw</a></div></div>"#,
        owner = html_escape(owner),
        repo = html_escape(repo_name),
        ref_name = html_escape(ref_name),
        path = path,
        filename = html_escape(filename),
        size = size,
    ));

    // File content
    let is_bin = content.len().min(8192) > 0 && content[..content.len().min(8192)].contains(&0);
    if is_bin {
        html.push_str(r#"<div class="file-content"><pre>Binary file not shown.</pre></div>"#);
    } else {
        let text = String::from_utf8_lossy(&content);
        let lang_class = lang_from_filename(filename);
        html.push_str(&format!(
            r#"<div class="file-content"><pre><code class="{lang}">{code}</code></pre></div>"#,
            lang = lang_class,
            code = html_escape(&text),
        ));
    }

    html_response(&layout(
        filename, owner, repo_name, ref_name, actor_name, &html,
    ))
}

// ---------------------------------------------------------------------------
// Raw file serving
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Page: Repo settings (/:owner/:repo/settings) — owner only
// ---------------------------------------------------------------------------

pub fn page_settings(
    sql: &SqlStorage,
    owner: &str,
    repo_name: &str,
    actor_name: Option<&str>,
) -> Result<Response> {
    // Stats — use sql.database_size() for DB size (Cloudflare's DO-native API,
    // not a raw PRAGMA which is blocked by the DO SQLite authorizer)
    #[derive(serde::Deserialize)]
    struct N {
        n: i64,
    }

    let commits = sql
        .exec("SELECT COUNT(*) AS n FROM commits", None)?
        .to_array::<N>()?
        .first()
        .map(|r| r.n)
        .unwrap_or(0);
    let blobs = sql
        .exec("SELECT COUNT(*) AS n FROM blobs", None)?
        .to_array::<N>()?
        .first()
        .map(|r| r.n)
        .unwrap_or(0);
    let db_bytes = sql.database_size();
    let db_mb = db_bytes as f64 / 1_048_576.0;

    let default_branch =
        store::get_config(sql, "default_branch")?.unwrap_or_else(|| "refs/heads/main".to_string());

    let action = |sub: &str| format!("/{}/{}/settings/{}", owner, repo_name, sub);

    let mut html = String::new();

    // Stats
    html.push_str(&format!(r#"
<section class="settings-section">
  <h2>Repository stats</h2>
  <div class="stats-grid">
    <div class="stat-box"><div class="stat-val">{commits}</div><div class="stat-lbl">commits</div></div>
    <div class="stat-box"><div class="stat-val">{blobs}</div><div class="stat-lbl">blobs</div></div>
    <div class="stat-box"><div class="stat-val">{db_mb:.1} MB</div><div class="stat-lbl">database size</div></div>
  </div>
</section>"#,
        commits = commits,
        blobs = blobs,
        db_mb = db_mb,
    ));

    // Index management
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
        commit_graph = action("rebuild-graph"),
        fts_commits = action("rebuild-fts-commits"),
        fts_head = action("rebuild-fts"),
    ));

    // Default branch config
    html.push_str(&format!(r#"
<section class="settings-section">
  <h2>Default branch</h2>
  <form method="POST" action="{action}" class="inline-form">
    <input type="text" name="branch" value="{branch}" class="branch-input" placeholder="refs/heads/main">
    <button class="btn-action" type="submit">Save</button>
  </form>
</section>"#,
        action = action("default-branch"),
        branch = html_escape(&default_branch),
    ));

    // Danger zone
    html.push_str(&format!(r#"
<section class="settings-section settings-danger">
  <h2>Danger zone</h2>
  <p class="settings-hint">This will permanently delete all data for <strong>{owner}/{repo}</strong>. There is no undo.</p>
  <form method="POST" action="{action}" class="inline-form">
    <input type="text" name="confirm" placeholder='Type "{owner}/{repo}" to confirm' class="branch-input danger-confirm">
    <button class="btn-danger-action" type="submit">Delete repository</button>
  </form>
</section>"#,
        owner = html_escape(owner),
        repo = html_escape(repo_name),
        action = action("delete"),
    ));

    html_response(&layout(
        "Settings",
        owner,
        repo_name,
        &default_branch,
        actor_name,
        &html,
    ))
}

// ---------------------------------------------------------------------------
// Page: Owner profile (/:owner/)
// Handled at the Worker level since there is no per-owner DO.
// ---------------------------------------------------------------------------

pub fn page_owner_profile(
    owner: &str,
    actor_name: Option<&str>,
    url: &Url,
    repos: &[String],
) -> Result<Response> {
    let is_owner = actor_name == Some(owner);
    let host = url.host_str().unwrap_or("your-worker.dev");
    let scheme = url.scheme();

    // Repo list (shown to everyone, empty state differs for owner vs visitor)
    let repo_list = if repos.is_empty() {
        if is_owner {
            format!(
                r#"<div class="push-box">
  <h2>Create your first repo</h2>
  <p>Repos are created on first push. Pick any name:</p>
  <pre class="push-cmd">cd my-project
git init
git add .
git commit -m "initial commit"
git remote add origin {scheme}://{owner}:TOKEN@{host}/{owner}/my-project
git push origin main</pre>
  <p class="push-note">Get a <code>TOKEN</code> from <a href="/settings">Settings → Access Tokens</a>.</p>
</div>"#,
                owner = html_escape(owner),
                scheme = scheme,
                host = host,
            )
        } else {
            r#"<p class="muted">No public repositories.</p>"#.to_string()
        }
    } else {
        let mut items = String::new();
        for repo in repos {
            items.push_str(&format!(
                r#"<li class="repo-item">
  <a href="/{owner}/{repo}" class="repo-link">{repo}</a>
</li>"#,
                owner = html_escape(owner),
                repo = html_escape(repo),
            ));
        }
        let mut html = format!(r#"<ul class="repo-list">{}</ul>"#, items);
        if is_owner {
            html.push_str(&format!(
                r#"<details class="push-box push-box-collapsed">
  <summary>Push a new repo</summary>
  <pre class="push-cmd" style="margin-top:12px">cd my-project
git init &amp;&amp; git add . &amp;&amp; git commit -m "initial commit"
git remote add origin {scheme}://{owner}:TOKEN@{host}/{owner}/my-project
git push origin main</pre>
  <p class="push-note">Get a <code>TOKEN</code> from <a href="/settings">Settings</a>.</p>
</details>"#,
                owner = html_escape(owner),
                scheme = scheme,
                host = host,
            ));
        }
        html
    };

    let body = format!(
        r#"<div class="profile-wrap">
<div class="profile-header">
  <h1 class="profile-title">{owner}</h1>
  {settings_link}
</div>
{repo_list}
</div>"#,
        owner = html_escape(owner),
        settings_link = if is_owner {
            r#"<a href="/settings" class="profile-settings">Settings</a>"#.to_string()
        } else {
            String::new()
        },
        repo_list = repo_list,
    );

    let html = format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>{owner} — ripgit</title>
<style>
  *{{margin:0;padding:0;box-sizing:border-box}}
  body{{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;font-size:14px;color:#1f2328;background:#fff}}
  .topbar{{background:#fff;border-bottom:1px solid #d1d9e0;padding:12px 24px;display:flex;align-items:center;gap:8px}}
  .topbar a{{color:#1f2328;text-decoration:none;font-weight:600}}
  .topbar .sep{{color:#656d76}}
  .profile-wrap{{max-width:800px;margin:40px auto;padding:0 24px}}
  .profile-header{{display:flex;align-items:baseline;justify-content:space-between;margin-bottom:24px}}
  .profile-title{{font-size:28px}}
  .profile-settings{{color:#656d76;font-size:13px;text-decoration:none}}
  .profile-settings:hover{{color:#0969da}}
  .repo-list{{list-style:none;border:1px solid #d1d9e0;border-radius:6px;overflow:hidden}}
  .repo-item{{border-bottom:1px solid #d1d9e0;padding:12px 16px}}
  .repo-item:last-child{{border-bottom:none}}
  .repo-link{{font-weight:600;color:#0969da;text-decoration:none;font-size:15px}}
  .repo-link:hover{{text-decoration:underline}}
  .push-box{{margin-top:20px;background:#f6f8fa;border:1px solid #d1d9e0;border-radius:6px;padding:20px}}
  .push-box-collapsed{{cursor:pointer}}
  .push-box-collapsed summary{{font-size:13px;color:#656d76;font-weight:600}}
  .push-box h2{{font-size:16px;margin-bottom:8px}}
  .push-cmd{{background:#fff;border:1px solid #d1d9e0;border-radius:6px;padding:14px 16px;font-size:13px;margin:12px 0;overflow-x:auto;line-height:1.6;font-family:ui-monospace,monospace}}
  .push-note{{font-size:13px;color:#656d76;margin-top:6px}}
  .push-note a{{color:#0969da}}
  .muted{{color:#656d76}}
  code{{background:#f6f8fa;border:1px solid #d1d9e0;border-radius:3px;padding:1px 5px;font-size:12px;font-family:ui-monospace,monospace}}
  p{{line-height:1.5}}
</style>
</head>
<body>
<div class="topbar">
  <a href="/">ripgit</a>
  <span class="sep">/</span>
  <span>{owner}</span>
</div>
{body}
</body>
</html>"#,
        owner = html_escape(owner),
        body = body,
    );

    let mut resp = Response::from_bytes(html.into_bytes())?;
    resp.headers_mut()
        .set("Content-Type", "text/html; charset=utf-8")?;
    Ok(resp)
}

// ---------------------------------------------------------------------------
// Raw file serving
// ---------------------------------------------------------------------------

pub fn serve_raw(sql: &SqlStorage, ref_name: &str, path: &str) -> Result<Response> {
    if path.is_empty() {
        return Response::error("path required", 400);
    }

    let commit_hash = match api::resolve_ref(sql, ref_name)? {
        Some(h) => h,
        None => return Response::error("ref not found", 404),
    };

    let blob_hash = match resolve_path_to_blob(sql, &commit_hash, path) {
        Ok(h) => h,
        Err(_) => return Response::error("Not Found", 404),
    };

    let content = load_blob(sql, &blob_hash)?;
    let filename = path.rsplit('/').next().unwrap_or(path);
    let content_type = raw_content_type(filename, &content);

    let mut resp = Response::from_bytes(content)?;
    resp.headers_mut().set("Content-Type", content_type)?;
    // Prevent the browser from sniffing or executing the content
    resp.headers_mut()
        .set("X-Content-Type-Options", "nosniff")?;
    resp.headers_mut()
        .set("Content-Security-Policy", "default-src 'none'")?;
    Ok(resp)
}

/// Pick a safe Content-Type for raw file serving.
/// Text files are served as text/plain (never text/html) so the browser
/// displays them rather than executing anything. Images get their proper type
/// so they can be embedded/viewed directly. Everything else that contains
/// null bytes is treated as binary.
fn raw_content_type(filename: &str, content: &[u8]) -> &'static str {
    let ext = filename.rsplit('.').next().unwrap_or("").to_lowercase();
    match ext.as_str() {
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "pdf" => "application/pdf",
        // SVG/HTML/XML/JSON: always text/plain — never execute in browser
        _ => {
            let is_binary = !content.is_empty() && content[..content.len().min(8192)].contains(&0);
            if is_binary {
                "application/octet-stream"
            } else {
                "text/plain; charset=utf-8"
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Page: Commit log
// ---------------------------------------------------------------------------

pub fn page_log(
    sql: &SqlStorage,
    owner: &str,
    repo_name: &str,
    url: &Url,
    actor_name: Option<&str>,
) -> Result<Response> {
    let ref_name = api::get_query(url, "ref").unwrap_or_else(|| {
        resolve_default_branch(sql)
            .map(|(name, _)| name)
            .unwrap_or_else(|_| "main".to_string())
    });
    let page: i64 = api::get_query(url, "page")
        .and_then(|v| v.parse().ok())
        .unwrap_or(1)
        .max(1);
    let per_page: i64 = 30;
    let offset = (page - 1) * per_page;

    let head = match api::resolve_ref(sql, &ref_name)? {
        Some(h) => h,
        None => {
            return html_response(&layout(
                "Commits",
                owner,
                repo_name,
                &ref_name,
                actor_name,
                "<p>No commits yet.</p>",
            ))
        }
    };

    // Walk commit chain
    let commits = walk_commits(sql, &head, per_page + 1, offset)?;
    let has_next = commits.len() as i64 > per_page;
    let display: Vec<_> = commits.into_iter().take(per_page as usize).collect();

    let mut html = String::new();

    // Branch selector
    html.push_str(&render_branch_selector(
        sql, owner, repo_name, &ref_name, "log", "",
    )?);

    html.push_str(&format!(
        r#"<h1>Commits on <strong>{}</strong></h1>"#,
        html_escape(&ref_name)
    ));

    html.push_str(r#"<ul class="commit-list">"#);
    for c in &display {
        html.push_str(&format!(
            r#"<li class="commit-item">
  <a class="commit-hash" href="/{owner}/{repo}/commit/{hash}">{short}</a>
  <span class="commit-msg"><a href="/{owner}/{repo}/commit/{hash}">{msg}</a></span>
  <span class="commit-author">{author}</span>
  <span class="commit-time">{time}</span>
</li>"#,
            owner = owner,
            repo = repo_name,
            hash = c.hash,
            short = &c.hash[..7.min(c.hash.len())],
            msg = html_escape(&first_line(&c.message)),
            author = html_escape(&c.author),
            time = format_time(c.commit_time),
        ));
    }
    html.push_str("</ul>");

    // Pagination
    html.push_str(r#"<div class="pagination">"#);
    if page > 1 {
        html.push_str(&format!(
            r#"<a href="/{}/{}/commits?ref={}&page={}">Previous</a>"#,
            owner,
            repo_name,
            ref_name,
            page - 1
        ));
    }
    if has_next {
        html.push_str(&format!(
            r#"<a href="/{}/{}/commits?ref={}&page={}">Next</a>"#,
            owner,
            repo_name,
            ref_name,
            page + 1
        ));
    }
    html.push_str("</div>");

    html_response(&layout(
        "Commits", owner, repo_name, &ref_name, actor_name, &html,
    ))
}

// ---------------------------------------------------------------------------
// Page: Commit detail with diff
// ---------------------------------------------------------------------------

pub fn page_commit(
    sql: &SqlStorage,
    owner: &str,
    repo_name: &str,
    hash: &str,
    actor_name: Option<&str>,
) -> Result<Response> {
    if hash.is_empty() {
        return Response::error("missing commit hash", 400);
    }

    // Load commit metadata
    let (default_branch, _) = resolve_default_branch(sql)?;
    let commit = load_commit_meta(sql, hash)?;
    let diff_result = diff::diff_commit(sql, hash, true, 3)?;

    let mut html = String::new();

    // Commit header
    html.push_str(&format!(
        r#"<h1 style="font-size:18px;margin-bottom:4px">{msg}</h1>"#,
        msg = html_escape(&first_line(&commit.message)),
    ));
    html.push_str(&format!(
        r#"<p style="color:#656d76;margin-bottom:16px">{author} &lt;{email}&gt; committed {time}</p>"#,
        author = html_escape(&commit.author),
        email = html_escape(&commit.author_email),
        time = format_time(commit.commit_time),
    ));

    // Full message if multi-line
    let rest = rest_of_message(&commit.message);
    if !rest.is_empty() {
        html.push_str(&format!(
            r#"<pre style="margin-bottom:16px;padding:12px;background:#f6f8fa;border-radius:6px;white-space:pre-wrap">{}</pre>"#,
            html_escape(&rest),
        ));
    }

    // Commit info
    html.push_str(&format!(
        r#"<div style="font-family:monospace;font-size:13px;margin-bottom:16px;color:#656d76">
  commit {hash}<br>
  {parents}
</div>"#,
        hash = hash,
        parents = if let Some(ref p) = diff_result.parent_hash {
            format!(
                r#"parent <a href="/{}/{}/commit/{}">{}</a>"#,
                owner,
                repo_name,
                p,
                &p[..7.min(p.len())]
            )
        } else {
            "root commit".to_string()
        },
    ));

    // Stats
    html.push_str(&format!(
        r#"<div class="diff-stats">
  Showing <strong>{files}</strong> changed file{s} with
  <span class="stat-add">+{add}</span> addition{as_} and
  <span class="stat-del">-{del}</span> deletion{ds}.
</div>"#,
        files = diff_result.stats.files_changed,
        s = if diff_result.stats.files_changed == 1 {
            ""
        } else {
            "s"
        },
        add = diff_result.stats.additions,
        as_ = if diff_result.stats.additions == 1 {
            ""
        } else {
            "s"
        },
        del = diff_result.stats.deletions,
        ds = if diff_result.stats.deletions == 1 {
            ""
        } else {
            "s"
        },
    ));

    // File diffs
    for file in &diff_result.files {
        html.push_str(&render_file_diff(file));
    }

    html_response(&layout(
        &format!("Commit {}", &hash[..7.min(hash.len())]),
        owner,
        repo_name,
        &default_branch,
        actor_name,
        &html,
    ))
}

// ---------------------------------------------------------------------------
// Page: Search
// ---------------------------------------------------------------------------

pub fn page_search(
    sql: &SqlStorage,
    owner: &str,
    repo_name: &str,
    url: &Url,
    actor_name: Option<&str>,
) -> Result<Response> {
    let raw_query = api::get_query(url, "q").unwrap_or_default();
    let scope_param = api::get_query(url, "scope").unwrap_or_else(|| "code".to_string());

    // Parse @prefix: tokens out of the query
    let parsed = api::parse_search_query(&raw_query);
    let query = parsed.fts_query.clone();
    let scope = parsed.scope.map(|s| s.to_string()).unwrap_or(scope_param);

    // Resolve default branch for blob links
    let (default_branch, _) = resolve_default_branch(sql)?;

    let mut html = String::new();
    html.push_str("<h1>Search</h1>");

    // Scope tabs
    let code_active = if scope == "code" {
        " style=\"font-weight:700;text-decoration:underline\""
    } else {
        ""
    };
    let commits_active = if scope == "commits" {
        " style=\"font-weight:700;text-decoration:underline\""
    } else {
        ""
    };
    html.push_str(&format!(
        r#"<div style="margin-bottom:12px;display:flex;gap:16px">
  <a href="/{owner}/{repo}/search-ui?q={q}&scope=code"{ca}>Code</a>
  <a href="/{owner}/{repo}/search-ui?q={q}&scope=commits"{cc}>Commits</a>
</div>"#,
        owner = owner,
        repo = repo_name,
        q = html_escape(&raw_query),
        ca = code_active,
        cc = commits_active,
    ));

    // Search form — single input, @prefix: syntax handles filtering
    html.push_str(&format!(
        r#"<form class="search-form" action="/{owner}/{repo}/search-ui" method="get">
  <input type="hidden" name="scope" value="{scope}">
  <input type="text" name="q" value="{q}" placeholder="Search... (@author: @message: @path: @ext: @content:)">
  <button type="submit">Search</button>
</form>"#,
        owner = owner,
        repo = repo_name,
        scope = html_escape(&scope),
        q = html_escape(&raw_query),
    ));

    if !query.is_empty() || !raw_query.is_empty() {
        let effective_query = if query.is_empty() { &raw_query } else { &query };
        if scope == "commits" {
            // Commit search
            let results = store::search_commits(sql, effective_query, 50)?;
            if results.is_empty() {
                html.push_str("<p>No matching commits found.</p>");
            } else {
                html.push_str(&format!(
                    "<p>{} commit{} found</p>",
                    results.len(),
                    if results.len() == 1 { "" } else { "s" }
                ));
                html.push_str(r#"<ul class="commit-list">"#);
                for c in &results {
                    html.push_str(&format!(
                        r#"<li class="commit-item">
  <a class="commit-hash" href="/{owner}/{repo}/commit/{hash}">{short}</a>
  <span class="commit-msg"><a href="/{owner}/{repo}/commit/{hash}">{msg}</a></span>
  <span class="commit-author">{author}</span>
  <span class="commit-time">{time}</span>
</li>"#,
                        owner = owner,
                        repo = repo_name,
                        hash = c.hash,
                        short = &c.hash[..7.min(c.hash.len())],
                        msg = html_escape(&first_line(&c.message)),
                        author = html_escape(&c.author),
                        time = format_time(c.commit_time),
                    ));
                }
                html.push_str("</ul>");
            }
        } else {
            // Code search
            let effective_query = if query.is_empty() { &raw_query } else { &query };
            let results = store::search_code(
                sql,
                effective_query,
                parsed.path_filter.as_deref(),
                parsed.ext_filter.as_deref(),
                50,
            )?;
            let total_matches: usize = results.iter().map(|r| r.matches.len()).sum();

            if results.is_empty() {
                html.push_str("<p>No results found.</p>");
            } else {
                html.push_str(&format!(
                    "<p>{} match{} across {} file{}</p>",
                    total_matches,
                    if total_matches == 1 { "" } else { "es" },
                    results.len(),
                    if results.len() == 1 { "" } else { "s" },
                ));

                for r in &results {
                    html.push_str(r#"<div class="search-result">"#);
                    html.push_str(&format!(
                        r#"<div class="search-result-path"><a href="/{owner}/{repo}/blob/{branch}/{path}">{path}</a> ({n} match{s})</div>"#,
                        owner = owner,
                        repo = repo_name,
                        branch = default_branch,
                        path = html_escape(&r.path),
                        n = r.matches.len(),
                        s = if r.matches.len() == 1 { "" } else { "es" },
                    ));

                    // Show matching lines with line numbers
                    html.push_str(r#"<table class="diff-table" style="margin-top:4px">"#);
                    for m in &r.matches {
                        html.push_str(&format!(
                            r#"<tr class="diff-line-add"><td class="diff-ln"><a href="/{owner}/{repo}/blob/{branch}/{path}#L{ln}" style="color:#656d76">{ln}</a></td><td>{text}</td></tr>"#,
                            owner = owner,
                            repo = repo_name,
                            branch = default_branch,
                            path = html_escape(&r.path),
                            ln = m.line_number,
                            text = html_escape(&m.line_text),
                        ));
                    }
                    html.push_str("</table>");
                    html.push_str("</div>");
                }
            }
        }
    }

    html_response(&layout(
        "Search",
        owner,
        repo_name,
        &default_branch,
        actor_name,
        &html,
    ))
}

// ---------------------------------------------------------------------------
// Diff rendering
// ---------------------------------------------------------------------------

pub(crate) fn render_file_diff(file: &diff::FileDiff) -> String {
    let status_class = match file.status {
        diff::DiffStatus::Added => "added",
        diff::DiffStatus::Deleted => "deleted",
        diff::DiffStatus::Modified => "modified",
    };
    let status_label = match file.status {
        diff::DiffStatus::Added => "A",
        diff::DiffStatus::Deleted => "D",
        diff::DiffStatus::Modified => "M",
    };

    let mut html = String::new();
    html.push_str(r#"<div class="diff-file">"#);
    html.push_str(&format!(
        r#"<div class="diff-file-header"><span class="diff-status {cls}">{label}</span> {path}</div>"#,
        cls = status_class,
        label = status_label,
        path = html_escape(&file.path),
    ));

    if let Some(hunks) = &file.hunks {
        for hunk in hunks {
            // Check for binary marker
            if hunk.lines.len() == 1 && hunk.lines[0].tag == "binary" {
                html.push_str(r#"<div class="diff-hunk-header">Binary files differ</div>"#);
                continue;
            }

            html.push_str(&format!(
                r#"<div class="diff-hunk-header">@@ -{},{} +{},{} @@</div>"#,
                hunk.old_start, hunk.old_count, hunk.new_start, hunk.new_count,
            ));

            html.push_str(r#"<table class="diff-table">"#);
            let mut old_ln = hunk.old_start;
            let mut new_ln = hunk.new_start;

            for line in &hunk.lines {
                let (class, prefix, oln, nln) = match line.tag {
                    "add" => {
                        let n = new_ln;
                        new_ln += 1;
                        ("diff-line-add", "+", String::new(), n.to_string())
                    }
                    "delete" => {
                        let o = old_ln;
                        old_ln += 1;
                        ("diff-line-del", "-", o.to_string(), String::new())
                    }
                    _ => {
                        let o = old_ln;
                        let n = new_ln;
                        old_ln += 1;
                        new_ln += 1;
                        ("diff-line-ctx", " ", o.to_string(), n.to_string())
                    }
                };

                // Strip trailing newline for display
                let content = line.content.trim_end_matches('\n');

                html.push_str(&format!(
                    r#"<tr class="{cls}"><td class="diff-ln">{oln}</td><td class="diff-ln">{nln}</td><td>{prefix}{content}</td></tr>"#,
                    cls = class,
                    oln = oln,
                    nln = nln,
                    prefix = prefix,
                    content = html_escape(content),
                ));
            }
            html.push_str("</table>");
        }
    }

    html.push_str("</div>");
    html
}

// ---------------------------------------------------------------------------
// Commit list rendering (shared by home + log)
// ---------------------------------------------------------------------------

fn render_commit_list(
    sql: &SqlStorage,
    head: &str,
    limit: i64,
    owner: &str,
    repo_name: &str,
) -> Result<String> {
    let commits = walk_commits(sql, head, limit, 0)?;
    let mut html = String::new();
    html.push_str(r#"<ul class="commit-list">"#);
    for c in &commits {
        html.push_str(&format!(
            r#"<li class="commit-item">
  <a class="commit-hash" href="/{owner}/{repo}/commit/{hash}">{short}</a>
  <span class="commit-msg"><a href="/{owner}/{repo}/commit/{hash}">{msg}</a></span>
  <span class="commit-time">{time}</span>
</li>"#,
            owner = owner,
            repo = repo_name,
            hash = c.hash,
            short = &c.hash[..7.min(c.hash.len())],
            msg = html_escape(&first_line(&c.message)),
            time = format_time(c.commit_time),
        ));
    }
    html.push_str("</ul>");
    Ok(html)
}

// ---------------------------------------------------------------------------
// README rendering
// ---------------------------------------------------------------------------

fn render_readme(
    sql: &SqlStorage,
    tree_hash: &str,
    owner: &str,
    repo_name: &str,
    ref_name: &str,
) -> Result<String> {
    // Look for README files in the root tree
    let entries = load_sorted_tree(sql, tree_hash)?;
    let readme = entries.iter().find(|e| {
        let lower = e.name.to_lowercase();
        lower == "readme.md" || lower == "readme" || lower == "readme.txt"
    });

    let entry = match readme {
        Some(e) => e,
        None => return Ok(String::new()),
    };

    if entry.is_tree {
        return Ok(String::new());
    }

    let content = load_blob(sql, &entry.hash)?;
    let text = match std::str::from_utf8(&content) {
        Ok(t) => t,
        Err(_) => return Ok(String::new()),
    };

    let mut html = String::new();
    html.push_str(r#"<div class="readme-box">"#);
    html.push_str(&format!(
        r#"<div class="readme-header"><a href="/{}/{}/blob/{}/{}">{}</a></div>"#,
        owner,
        repo_name,
        ref_name,
        entry.name,
        html_escape(&entry.name),
    ));
    html.push_str(r#"<div class="readme-body">"#);

    let is_md = entry.name.to_lowercase().ends_with(".md");
    if is_md {
        html.push_str(&render_markdown(text));
    } else {
        html.push_str(&format!("<pre>{}</pre>", html_escape(text)));
    }

    html.push_str("</div></div>");
    Ok(html)
}

// ---------------------------------------------------------------------------
// Breadcrumb rendering
// ---------------------------------------------------------------------------

fn render_breadcrumb(
    owner: &str,
    repo_name: &str,
    ref_name: &str,
    path: &str,
    is_tree: bool,
) -> String {
    let mut html = String::from(r#"<div class="breadcrumb">"#);
    html.push_str(&format!(
        r#"<a href="/{owner}/{repo}/">{repo}</a> / "#,
        owner = owner,
        repo = repo_name,
    ));

    if path.is_empty() {
        html.push_str("</div>");
        return html;
    }

    let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    for (i, part) in parts.iter().enumerate() {
        if i < parts.len() - 1 {
            let sub_path = parts[..=i].join("/");
            html.push_str(&format!(
                r#"<a href="/{}/{}/tree/{}/{}">{}</a> / "#,
                owner,
                repo_name,
                ref_name,
                sub_path,
                html_escape(part),
            ));
        } else {
            // Last segment
            if is_tree {
                html.push_str(&format!("<strong>{}</strong>", html_escape(part),));
            } else {
                html.push_str(&format!("<strong>{}</strong>", html_escape(part),));
            }
        }
    }

    html.push_str("</div>");
    html
}

// ---------------------------------------------------------------------------
// Data loading helpers
// ---------------------------------------------------------------------------

struct SortedEntry {
    name: String,
    hash: String,
    is_tree: bool,
}

fn load_sorted_tree(sql: &SqlStorage, tree_hash: &str) -> Result<Vec<SortedEntry>> {
    let entries = store::load_tree_from_db(sql, tree_hash)?;

    let mut dirs: Vec<SortedEntry> = Vec::new();
    let mut files: Vec<SortedEntry> = Vec::new();

    for e in entries {
        let se = SortedEntry {
            name: e.name,
            hash: e.hash,
            is_tree: e.mode == 0o040000,
        };
        if se.is_tree {
            dirs.push(se);
        } else {
            files.push(se);
        }
    }

    dirs.sort_by(|a, b| a.name.cmp(&b.name));
    files.sort_by(|a, b| a.name.cmp(&b.name));
    dirs.append(&mut files);
    Ok(dirs)
}

fn load_tree_for_commit(sql: &SqlStorage, commit_hash: &str) -> Result<String> {
    #[derive(serde::Deserialize)]
    struct Row {
        tree_hash: String,
    }

    let rows: Vec<Row> = sql
        .exec(
            "SELECT tree_hash FROM commits WHERE hash = ?",
            vec![SqlStorageValue::from(commit_hash.to_string())],
        )?
        .to_array()?;

    rows.into_iter()
        .next()
        .map(|r| r.tree_hash)
        .ok_or_else(|| Error::RustError(format!("commit not found: {}", commit_hash)))
}

fn resolve_path_to_tree(sql: &SqlStorage, commit_hash: &str, path: &str) -> Result<String> {
    let root = load_tree_for_commit(sql, commit_hash)?;
    if path.is_empty() {
        return Ok(root);
    }

    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
    let mut current = root;

    for segment in &segments {
        let entry = find_tree_entry(sql, &current, segment)?;
        match entry {
            Some((hash, mode)) if mode == 0o040000 => {
                current = hash;
            }
            Some(_) => {
                return Err(Error::RustError(format!(
                    "'{}' is not a directory",
                    segment
                )));
            }
            None => {
                return Err(Error::RustError(format!("'{}' not found in tree", segment)));
            }
        }
    }

    Ok(current)
}

fn resolve_path_to_blob(sql: &SqlStorage, commit_hash: &str, path: &str) -> Result<String> {
    let root = load_tree_for_commit(sql, commit_hash)?;
    let segments: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();

    if segments.is_empty() {
        return Err(Error::RustError("empty path".into()));
    }

    let mut current_tree = root;

    for (i, segment) in segments.iter().enumerate() {
        let entry = find_tree_entry(sql, &current_tree, segment)?;
        match entry {
            Some((hash, mode)) => {
                if i < segments.len() - 1 {
                    if mode != 0o040000 {
                        return Err(Error::RustError(format!(
                            "'{}' is not a directory",
                            segment
                        )));
                    }
                    current_tree = hash;
                } else {
                    // Last segment — should be a blob
                    return Ok(hash);
                }
            }
            None => {
                return Err(Error::RustError(format!("'{}' not found", segment)));
            }
        }
    }

    Err(Error::RustError("path resolution failed".into()))
}

fn find_tree_entry(sql: &SqlStorage, tree_hash: &str, name: &str) -> Result<Option<(String, u32)>> {
    #[derive(serde::Deserialize)]
    struct Row {
        entry_hash: String,
        mode: i64,
    }

    let rows: Vec<Row> = sql
        .exec(
            "SELECT entry_hash, mode FROM trees WHERE tree_hash = ? AND name = ?",
            vec![
                SqlStorageValue::from(tree_hash.to_string()),
                SqlStorageValue::from(name.to_string()),
            ],
        )?
        .to_array()?;

    Ok(rows
        .into_iter()
        .next()
        .map(|r| (r.entry_hash, r.mode as u32)))
}

fn load_blob(sql: &SqlStorage, blob_hash: &str) -> Result<Vec<u8>> {
    #[derive(serde::Deserialize)]
    struct BlobInfo {
        group_id: i64,
        version_in_group: i64,
    }

    let rows: Vec<BlobInfo> = sql
        .exec(
            "SELECT group_id, version_in_group FROM blobs WHERE blob_hash = ?",
            vec![SqlStorageValue::from(blob_hash.to_string())],
        )?
        .to_array()?;

    match rows.into_iter().next() {
        Some(info) => store::reconstruct_blob(sql, info.group_id, info.version_in_group),
        None => Err(Error::RustError(format!("blob not found: {}", blob_hash))),
    }
}

struct CommitMeta {
    hash: String,
    author: String,
    author_email: String,
    commit_time: i64,
    message: String,
}

fn load_commit_meta(sql: &SqlStorage, hash: &str) -> Result<CommitMeta> {
    #[derive(serde::Deserialize)]
    struct Row {
        author: String,
        author_email: String,
        commit_time: i64,
        message: String,
    }

    let rows: Vec<Row> = sql
        .exec(
            "SELECT author, author_email, commit_time, message
             FROM commits WHERE hash = ?",
            vec![SqlStorageValue::from(hash.to_string())],
        )?
        .to_array()?;

    let row = rows
        .into_iter()
        .next()
        .ok_or_else(|| Error::RustError(format!("commit not found: {}", hash)))?;

    Ok(CommitMeta {
        hash: hash.to_string(),
        author: row.author,
        author_email: row.author_email,
        commit_time: row.commit_time,
        message: row.message,
    })
}

fn walk_commits(sql: &SqlStorage, head: &str, limit: i64, offset: i64) -> Result<Vec<CommitMeta>> {
    let mut result = Vec::new();
    let mut current = Some(head.to_string());
    let mut skipped = 0i64;

    while let Some(hash) = current {
        if result.len() as i64 >= limit {
            break;
        }

        let meta = match load_commit_meta_opt(sql, &hash)? {
            Some(m) => m,
            None => break,
        };

        // Get first parent for next iteration
        #[derive(serde::Deserialize)]
        struct ParentRow {
            parent_hash: String,
        }
        let parents: Vec<ParentRow> = sql
            .exec(
                "SELECT parent_hash FROM commit_parents
                 WHERE commit_hash = ? ORDER BY ordinal ASC LIMIT 1",
                vec![SqlStorageValue::from(hash.clone())],
            )?
            .to_array()?;

        current = parents.into_iter().next().map(|p| p.parent_hash);

        if skipped < offset {
            skipped += 1;
            continue;
        }

        result.push(meta);
    }

    Ok(result)
}

fn load_commit_meta_opt(sql: &SqlStorage, hash: &str) -> Result<Option<CommitMeta>> {
    #[derive(serde::Deserialize)]
    struct Row {
        author: String,
        author_email: String,
        commit_time: i64,
        message: String,
    }

    let rows: Vec<Row> = sql
        .exec(
            "SELECT author, author_email, commit_time, message
             FROM commits WHERE hash = ?",
            vec![SqlStorageValue::from(hash.to_string())],
        )?
        .to_array()?;

    Ok(rows.into_iter().next().map(|row| CommitMeta {
        hash: hash.to_string(),
        author: row.author,
        author_email: row.author_email,
        commit_time: row.commit_time,
        message: row.message,
    }))
}

pub(crate) fn resolve_default_branch(sql: &SqlStorage) -> Result<(String, Option<String>)> {
    // Try main first, then the first ref we find
    if let Some(hash) = api::resolve_ref(sql, "main")? {
        return Ok(("main".to_string(), Some(hash)));
    }
    if let Some(hash) = api::resolve_ref(sql, "master")? {
        return Ok(("master".to_string(), Some(hash)));
    }

    // Fall back to first branch
    #[derive(serde::Deserialize)]
    struct Row {
        name: String,
        commit_hash: String,
    }
    let rows: Vec<Row> = sql
        .exec(
            "SELECT name, commit_hash FROM refs
             WHERE name LIKE 'refs/heads/%' LIMIT 1",
            None,
        )?
        .to_array()?;

    match rows.into_iter().next() {
        Some(r) => {
            let branch = r
                .name
                .strip_prefix("refs/heads/")
                .unwrap_or(&r.name)
                .to_string();
            Ok((branch, Some(r.commit_hash)))
        }
        None => Ok(("main".to_string(), None)),
    }
}

// ---------------------------------------------------------------------------
// Branch selector
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

/// Render a branch dropdown. `page_type` is "home", "tree", "blob", or "log".
/// For tree/blob, `path` is the current file path. Navigates on change.
fn render_branch_selector(
    sql: &SqlStorage,
    owner: &str,
    repo_name: &str,
    current_ref: &str,
    page_type: &str,
    path: &str,
) -> Result<String> {
    let branches = load_branches(sql)?;

    if branches.len() <= 1 {
        // Single branch — just show the name, no dropdown needed
        return Ok(format!(
            r#"<div class="branch-selector"><span class="branch-label">branch:</span> <strong>{}</strong></div>"#,
            html_escape(current_ref)
        ));
    }

    let mut html = String::from(
        r#"<div class="branch-selector"><span class="branch-label">branch:</span> <select onchange="window.location.href=this.value">"#,
    );

    for branch in &branches {
        let url = match page_type {
            "tree" => format!("/{}/{}/tree/{}/{}", owner, repo_name, branch, path),
            "blob" => format!("/{}/{}/blob/{}/{}", owner, repo_name, branch, path),
            "log" => format!("/{}/{}/commits?ref={}", owner, repo_name, branch),
            _ => format!("/{}/{}/?ref={}", owner, repo_name, branch), // home
        };
        let selected = if branch == current_ref {
            " selected"
        } else {
            ""
        };
        html.push_str(&format!(
            r#"<option value="{}"{}>{}</option>"#,
            url,
            selected,
            html_escape(branch)
        ));
    }

    html.push_str("</select></div>");
    Ok(html)
}

// ---------------------------------------------------------------------------
// Markdown renderer (minimal, covers typical READMEs)
// ---------------------------------------------------------------------------

pub(crate) fn render_markdown(text: &str) -> String {
    let mut html = String::new();
    let mut in_code_block = false;
    let mut in_list = false;

    for line in text.lines() {
        // Code block fences
        if line.starts_with("```") {
            if in_code_block {
                html.push_str("</code></pre>\n");
                in_code_block = false;
            } else {
                if in_list {
                    html.push_str("</ul>\n");
                    in_list = false;
                }
                let code_lang = line[3..].trim();
                let lang_cls = if code_lang.is_empty() {
                    String::new()
                } else {
                    format!(" class=\"language-{}\"", html_escape(code_lang))
                };
                html.push_str(&format!("<pre><code{}>\n", lang_cls));
                in_code_block = true;
            }
            continue;
        }

        if in_code_block {
            html.push_str(&html_escape(line));
            html.push('\n');
            continue;
        }

        // Headings
        if line.starts_with("### ") {
            close_list(&mut html, &mut in_list);
            html.push_str(&format!("<h3>{}</h3>\n", inline_md(line[4..].trim())));
        } else if line.starts_with("## ") {
            close_list(&mut html, &mut in_list);
            html.push_str(&format!("<h2>{}</h2>\n", inline_md(line[3..].trim())));
        } else if line.starts_with("# ") {
            close_list(&mut html, &mut in_list);
            html.push_str(&format!("<h1>{}</h1>\n", inline_md(line[2..].trim())));
        }
        // Horizontal rule
        else if line.trim() == "---" || line.trim() == "***" || line.trim() == "___" {
            close_list(&mut html, &mut in_list);
            html.push_str("<hr>\n");
        }
        // Unordered list
        else if line.starts_with("- ") || line.starts_with("* ") {
            if !in_list {
                html.push_str("<ul>\n");
                in_list = true;
            }
            html.push_str(&format!("<li>{}</li>\n", inline_md(line[2..].trim())));
        }
        // Blockquote
        else if line.starts_with("> ") {
            close_list(&mut html, &mut in_list);
            html.push_str(&format!(
                "<blockquote>{}</blockquote>\n",
                inline_md(line[2..].trim())
            ));
        }
        // Empty line
        else if line.trim().is_empty() {
            close_list(&mut html, &mut in_list);
        }
        // Paragraph
        else {
            close_list(&mut html, &mut in_list);
            html.push_str(&format!("<p>{}</p>\n", inline_md(line)));
        }
    }

    close_list(&mut html, &mut in_list);
    if in_code_block {
        html.push_str("</code></pre>\n");
    }

    html
}

fn close_list(html: &mut String, in_list: &mut bool) {
    if *in_list {
        html.push_str("</ul>\n");
        *in_list = false;
    }
}

/// Process inline markdown: bold, italic, code, links.
fn inline_md(text: &str) -> String {
    let escaped = html_escape(text);
    let mut result = String::with_capacity(escaped.len());
    let bytes = escaped.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        // Inline code: `code`
        if bytes[i] == b'`' {
            if let Some(end) = escaped[i + 1..].find('`') {
                result.push_str("<code>");
                result.push_str(&escaped[i + 1..i + 1 + end]);
                result.push_str("</code>");
                i += end + 2;
                continue;
            }
        }

        // Bold: **text**
        if i + 1 < len && bytes[i] == b'*' && bytes[i + 1] == b'*' {
            if let Some(end) = escaped[i + 2..].find("**") {
                result.push_str("<strong>");
                result.push_str(&escaped[i + 2..i + 2 + end]);
                result.push_str("</strong>");
                i += end + 4;
                continue;
            }
        }

        // Italic: *text* (single asterisk, not preceded by another *)
        if bytes[i] == b'*' && (i == 0 || bytes[i - 1] != b'*') {
            if let Some(end) = escaped[i + 1..].find('*') {
                if end > 0 && (i + 1 + end + 1 >= len || bytes[i + 1 + end + 1] != b'*') {
                    result.push_str("<em>");
                    result.push_str(&escaped[i + 1..i + 1 + end]);
                    result.push_str("</em>");
                    i += end + 2;
                    continue;
                }
            }
        }

        // Links: [text](url) — operates on escaped text, so ( is literal
        if bytes[i] == b'[' {
            if let Some(close_bracket) = escaped[i + 1..].find(']') {
                let after = i + 1 + close_bracket + 1;
                if after < len && bytes[after] == b'(' {
                    if let Some(close_paren) = escaped[after + 1..].find(')') {
                        let link_text = &escaped[i + 1..i + 1 + close_bracket];
                        let url = &escaped[after + 1..after + 1 + close_paren];
                        result.push_str(&format!("<a href=\"{}\">{}</a>", url, link_text));
                        i = after + 1 + close_paren + 1;
                        continue;
                    }
                }
            }
        }

        result.push(bytes[i] as char);
        i += 1;
    }

    result
}

// ---------------------------------------------------------------------------
// Utility functions
// ---------------------------------------------------------------------------

pub(crate) fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

pub(crate) fn html_response(body: &str) -> Result<Response> {
    let mut resp = Response::from_html(body)?;
    resp.headers_mut().set("Cache-Control", "no-cache")?;
    Ok(resp)
}

fn first_line(s: &str) -> String {
    s.lines().next().unwrap_or("").to_string()
}

fn rest_of_message(s: &str) -> String {
    let mut lines = s.lines();
    lines.next(); // skip first line
    let rest: String = lines.collect::<Vec<_>>().join("\n");
    rest.trim().to_string()
}

fn parent_path(path: &str) -> String {
    match path.rfind('/') {
        Some(pos) => path[..pos].to_string(),
        None => String::new(),
    }
}

pub(crate) fn format_time(unix: i64) -> String {
    // Simple relative time
    let now = js_sys_date_now() / 1000.0;
    let diff = (now as i64) - unix;

    if diff < 60 {
        "just now".to_string()
    } else if diff < 3600 {
        let m = diff / 60;
        format!("{} minute{} ago", m, if m == 1 { "" } else { "s" })
    } else if diff < 86400 {
        let h = diff / 3600;
        format!("{} hour{} ago", h, if h == 1 { "" } else { "s" })
    } else if diff < 2592000 {
        let d = diff / 86400;
        format!("{} day{} ago", d, if d == 1 { "" } else { "s" })
    } else if diff < 31536000 {
        let m = diff / 2592000;
        format!("{} month{} ago", m, if m == 1 { "" } else { "s" })
    } else {
        let y = diff / 31536000;
        format!("{} year{} ago", y, if y == 1 { "" } else { "s" })
    }
}

fn js_sys_date_now() -> f64 {
    // In Cloudflare Workers, Date.now() returns the time at request start.
    // We can't use js_sys directly without adding it as a dependency.
    // Instead, use worker::Date.
    worker::Date::now().as_millis() as f64
}

fn lang_from_filename(name: &str) -> String {
    let ext = name.rsplit('.').next().unwrap_or("");
    let lang = match ext {
        "rs" => "language-rust",
        "js" | "mjs" | "cjs" => "language-javascript",
        "ts" | "mts" | "cts" => "language-typescript",
        "py" => "language-python",
        "rb" => "language-ruby",
        "go" => "language-go",
        "java" => "language-java",
        "c" | "h" => "language-c",
        "cpp" | "cc" | "cxx" | "hpp" => "language-cpp",
        "cs" => "language-csharp",
        "swift" => "language-swift",
        "kt" | "kts" => "language-kotlin",
        "php" => "language-php",
        "sh" | "bash" | "zsh" => "language-bash",
        "json" => "language-json",
        "yaml" | "yml" => "language-yaml",
        "toml" => "language-toml",
        "xml" | "svg" | "html" | "htm" => "language-xml",
        "css" => "language-css",
        "scss" | "sass" => "language-scss",
        "sql" => "language-sql",
        "md" | "markdown" => "language-markdown",
        "dockerfile" | "Dockerfile" => "language-dockerfile",
        "makefile" | "Makefile" => "language-makefile",
        "zig" => "language-zig",
        "lua" => "language-lua",
        "r" | "R" => "language-r",
        "ex" | "exs" => "language-elixir",
        "erl" | "hrl" => "language-erlang",
        "hs" => "language-haskell",
        "ml" | "mli" => "language-ocaml",
        "nix" => "language-nix",
        _ => "",
    };
    lang.to_string()
}
