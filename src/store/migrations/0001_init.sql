CREATE TABLE repo_meta (
    id              INTEGER PRIMARY KEY CHECK (id = 1),
    owner           TEXT NOT NULL,
    repo            TEXT NOT NULL,
    padding_width   INTEGER NOT NULL DEFAULT 4,
    last_full_sync_at INTEGER
);

CREATE TABLE issues (
    node_id      TEXT PRIMARY KEY,
    number       INTEGER NOT NULL UNIQUE,
    title        TEXT NOT NULL,
    state        TEXT NOT NULL,
    state_reason TEXT,
    author       TEXT,
    body         TEXT NOT NULL,
    created_at   INTEGER NOT NULL,
    updated_at   INTEGER NOT NULL,
    closed_at    INTEGER,
    milestone    TEXT,
    labels_json  TEXT NOT NULL DEFAULT '[]',
    assignees_json TEXT NOT NULL DEFAULT '[]',
    deleted      INTEGER NOT NULL DEFAULT 0
);
CREATE INDEX idx_issues_updated ON issues(updated_at);

CREATE TABLE comments (
    node_id       TEXT PRIMARY KEY,
    issue_node_id TEXT NOT NULL REFERENCES issues(node_id) ON DELETE CASCADE,
    author        TEXT,
    created_at    INTEGER NOT NULL,
    body          TEXT NOT NULL
);
CREATE INDEX idx_comments_issue ON comments(issue_node_id);

CREATE TABLE cross_refs (
    issue_node_id           TEXT NOT NULL,
    referenced_issue_number INTEGER NOT NULL,
    event_type              TEXT NOT NULL,
    created_at              INTEGER NOT NULL,
    is_active               INTEGER NOT NULL DEFAULT 1,
    PRIMARY KEY (issue_node_id, referenced_issue_number, event_type, created_at)
);
CREATE INDEX idx_xref_issue ON cross_refs(issue_node_id);

CREATE TABLE labels (
    node_id     TEXT PRIMARY KEY,
    name        TEXT NOT NULL,
    color       TEXT NOT NULL,
    description TEXT
);

CREATE TABLE milestones (
    node_id     TEXT PRIMARY KEY,
    number      INTEGER NOT NULL,
    title       TEXT NOT NULL,
    state       TEXT NOT NULL,
    description TEXT,
    due_on      INTEGER
);

CREATE TABLE etags (
    resource TEXT PRIMARY KEY,
    etag     TEXT NOT NULL
);

CREATE TABLE sync_state (
    entity_type      TEXT PRIMARY KEY,
    updated_watermark INTEGER,
    resume_cursor    TEXT,
    run_phase        TEXT NOT NULL DEFAULT 'idle'
);
