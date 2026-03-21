//! Server-rendered HTML pages for issues and pull requests.

use worker::*;

type Url = worker::Url;

mod detail;
mod forms;
mod list;

pub(crate) use detail::{page_issue_detail, page_issue_detail_markdown};
pub(crate) use forms::{
    page_new_issue, page_new_issue_markdown, page_new_pull, page_new_pull_markdown,
};
pub(crate) use list::{
    page_issues_list, page_issues_list_markdown, page_pulls_list, page_pulls_list_markdown,
};

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
