CREATE TABLE issue_relationships (
    rel          TEXT NOT NULL CHECK (rel IN ('blocks', 'parent')),
    src_node_id  TEXT NOT NULL,
    dst_node_id  TEXT NOT NULL,
    position     INTEGER CHECK (rel = 'parent' OR position IS NULL),
    src_repo   TEXT,
    src_number INTEGER NOT NULL,
    src_state  TEXT NOT NULL,
    src_title  TEXT NOT NULL,
    dst_repo   TEXT,
    dst_number INTEGER NOT NULL,
    dst_state  TEXT NOT NULL,
    dst_title  TEXT NOT NULL,
    PRIMARY KEY (rel, src_node_id, dst_node_id)
);
CREATE INDEX idx_rel_src ON issue_relationships(src_node_id);
CREATE INDEX idx_rel_dst ON issue_relationships(dst_node_id);
CREATE UNIQUE INDEX idx_rel_one_parent ON issue_relationships(dst_node_id) WHERE rel = 'parent';

-- Backfill: force the next incremental sync to walk every issue once so
-- historical issues get relationship data. Pull-request state is untouched.
UPDATE sync_state
SET updated_watermark = NULL, resume_cursor = NULL, run_phase = 'idle'
WHERE entity_type = 'issues';
