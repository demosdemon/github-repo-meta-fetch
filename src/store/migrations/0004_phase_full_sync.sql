ALTER TABLE sync_state ADD COLUMN last_full_sync_at INTEGER;
ALTER TABLE sync_state ADD COLUMN last_reconciled_at INTEGER;

-- Backfill: a repository that already completed a --full run covering both
-- phases must not be surprised by another full walk on upgrade. A repository
-- that never ran --full yields NULL here and takes the implicit full walk,
-- which is the intended upgrade path.
--
-- last_reconciled_at gets no backfill: the old column recorded that a --full
-- run finished, never whether it was the fresh pass that also reconciled, so
-- there is nothing faithful to derive.
UPDATE sync_state
SET last_full_sync_at = (SELECT last_full_sync_at FROM repo_meta WHERE id = 1);

-- The per-phase markers are now the only record of full-walk history.
ALTER TABLE repo_meta DROP COLUMN last_full_sync_at;
