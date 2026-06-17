use std::fmt::Write as _;

/// One row in an issue index table.
#[derive(Debug, Clone)]
pub struct IndexRow {
    pub number: i64,
    pub title: String,
    pub state: String,
    pub assignees: Vec<String>,
    pub updated_at: String, // pre-formatted RFC3339
    pub file_rel: String,   // relative link target, e.g. "../0042.md"
}

/// One row in the labels document.
pub struct LabelRow {
    pub name: String,
    pub color: String,
    pub description: Option<String>,
    pub count: i64,
}

/// One row in the milestones document.
pub struct MilestoneRow {
    pub title: String,
    pub state: String,
    pub due_on: Option<String>, // pre-formatted date or None
    pub open: i64,
    pub closed: i64,
}

fn esc(s: &str) -> String {
    s.replace('|', "\\|")
}

/// A Markdown table sorted by issue number ascending.
#[must_use]
pub fn issue_table(rows: &[IndexRow]) -> String {
    let mut sorted = rows.to_vec();
    sorted.sort_by_key(|r| r.number);
    let mut out = String::new();
    writeln!(out, "| # | title | state | assignees | updated |").ok();
    writeln!(out, "| ---: | :--- | :--- | :--- | :--- |").ok();
    for r in &sorted {
        writeln!(
            out,
            "| [{}]({}) | {} | {} | {} | {} |",
            r.number,
            r.file_rel,
            esc(&r.title),
            r.state,
            esc(&r.assignees.join(", ")),
            r.updated_at,
        )
        .ok();
    }
    out
}

/// `labels.md` body from rows already joined with color/description/count.
#[must_use]
pub fn labels_doc(rows: &[LabelRow]) -> String {
    let mut out = String::from(
        "# Labels\n\n| name | color | count | description |\n| :--- | :--- | ---: | :--- |\n",
    );
    for r in rows {
        writeln!(
            out,
            "| {} | `#{}` | {} | {} |",
            esc(&r.name),
            r.color,
            r.count,
            esc(r.description.as_deref().unwrap_or(""))
        )
        .ok();
    }
    out
}

/// `milestones.md` body from rows with pre-computed open/closed counts.
#[must_use]
pub fn milestones_doc(rows: &[MilestoneRow]) -> String {
    use std::fmt::Write as _;
    let mut out = String::from(
        "# Milestones\n\n| title | state | due | open | closed |\n| :--- | :--- | :--- | ---: | ---: |\n",
    );
    for r in rows {
        let due = r.due_on.as_deref().unwrap_or("");
        writeln!(
            out,
            "| {} | {} | {} | {} | {} |",
            esc(&r.title),
            esc(&r.state),
            esc(due),
            r.open,
            r.closed
        )
        .ok();
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_sorted_and_escaped() {
        let rows = vec![
            IndexRow {
                number: 7,
                title: "b|ar".into(),
                state: "open".into(),
                assignees: vec!["x".into()],
                updated_at: "2026-06-10T00:00:00Z".into(),
                file_rel: "../0007.md".into(),
            },
            IndexRow {
                number: 3,
                title: "foo".into(),
                state: "closed".into(),
                assignees: vec![],
                updated_at: "2026-01-01T00:00:00Z".into(),
                file_rel: "../0003.md".into(),
            },
        ];
        let t = issue_table(&rows);
        let pos3 = t.find("0003.md").unwrap();
        let pos7 = t.find("0007.md").unwrap();
        assert!(pos3 < pos7, "rows must be sorted by number");
        assert!(t.contains("b\\|ar"), "pipes escaped");
    }

    #[test]
    fn labels_doc_has_counts() {
        let rows = vec![LabelRow {
            name: "bug".into(),
            color: "f00".into(),
            description: Some("a bug".into()),
            count: 5,
        }];
        let d = labels_doc(&rows);
        assert!(d.contains("| bug | `#f00` | 5 | a bug |"));
    }
}
