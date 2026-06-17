CREATE TABLE pull_requests (
    node_id        TEXT PRIMARY KEY,
    number         INTEGER NOT NULL UNIQUE,
    title          TEXT NOT NULL,
    state          TEXT NOT NULL,
    is_draft       INTEGER NOT NULL DEFAULT 0,
    merged         INTEGER NOT NULL DEFAULT 0,
    merged_at      INTEGER,
    merged_by      TEXT,
    base_ref       TEXT NOT NULL,
    head_ref       TEXT NOT NULL,
    additions      INTEGER NOT NULL DEFAULT 0,
    deletions      INTEGER NOT NULL DEFAULT 0,
    changed_files  INTEGER NOT NULL DEFAULT 0,
    author         TEXT,
    body           TEXT NOT NULL,
    created_at     INTEGER NOT NULL,
    updated_at     INTEGER NOT NULL,
    closed_at      INTEGER,
    milestone      TEXT,
    labels_json    TEXT NOT NULL DEFAULT '[]',
    assignees_json TEXT NOT NULL DEFAULT '[]',
    deleted        INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_prs_updated ON pull_requests(updated_at);

CREATE TABLE reviews (
    node_id      TEXT PRIMARY KEY,
    pr_node_id   TEXT NOT NULL,
    author       TEXT,
    state        TEXT NOT NULL,
    body         TEXT NOT NULL,
    submitted_at INTEGER
);
CREATE INDEX idx_reviews_pr ON reviews(pr_node_id);

CREATE TABLE review_threads (
    node_id     TEXT PRIMARY KEY,
    pr_node_id  TEXT NOT NULL,
    path        TEXT NOT NULL,
    line        INTEGER,
    is_resolved INTEGER NOT NULL DEFAULT 0,
    is_outdated INTEGER NOT NULL DEFAULT 0,
    diff_hunk   TEXT NOT NULL DEFAULT ''
);
CREATE INDEX idx_threads_pr ON review_threads(pr_node_id);

CREATE TABLE review_comments (
    node_id        TEXT PRIMARY KEY,
    thread_node_id TEXT NOT NULL,
    author         TEXT,
    created_at     INTEGER NOT NULL,
    body           TEXT NOT NULL
);
CREATE INDEX idx_review_comments_thread ON review_comments(thread_node_id);

-- Generalize comments: drop the FK-to-issues so PR conversation comments fit.
ALTER TABLE comments RENAME TO comments_old;
CREATE TABLE comments (
    node_id         TEXT PRIMARY KEY,
    subject_node_id TEXT NOT NULL,
    author          TEXT,
    created_at      INTEGER NOT NULL,
    body            TEXT NOT NULL
);
INSERT INTO comments (node_id, subject_node_id, author, created_at, body)
    SELECT node_id, issue_node_id, author, created_at, body FROM comments_old;
DROP TABLE comments_old;
CREATE INDEX idx_comments_subject ON comments(subject_node_id);
