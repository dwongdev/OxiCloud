-- ════════════════════════════════════════════════════════════════════════════
-- Share-link orphan cleanup: reverse cascade access_grants → storage.shares
-- ════════════════════════════════════════════════════════════════════════════
-- Today every share-link has two correlated rows:
--   1. `storage.shares`        — token + password hash + counters
--   2. `storage.access_grants` — permission rows for subject_type='token'
--
-- The forward direction is already wired (see 20260520000000_rebac_access_grants.sql):
--   DELETE storage.shares → trg_cleanup_grants_token → access_grants gone.
--
-- The reverse direction was not. A caller hitting `DELETE /api/grants/{id}`
-- on the last grant of a token would leave the storage.shares row stranded:
-- the token still resolves to "no access" (the cascade query finds no rows),
-- but the metadata row accumulates and never garbage-collects.
--
-- This migration adds an AFTER DELETE trigger on access_grants that, when
-- the deleted row's `subject_type='token'`, deletes the storage.shares row
-- iff no other grants reference that subject_id. Per-permission revokes
-- (deleting one of several grants for the same token) are unaffected.

CREATE OR REPLACE FUNCTION storage.cleanup_share_on_last_token_grant_delete()
RETURNS trigger AS $$
BEGIN
    IF OLD.subject_type = 'token' THEN
        -- DELETE is a no-op when the share row is already gone — e.g. when
        -- the original DELETE came from `storage.shares`, the forward
        -- cascade (`trg_cleanup_grants_token`) is already deleting these
        -- grant rows. The `NOT EXISTS` guard also makes the trigger safe
        -- for multi-grant tokens: the share only goes away when its last
        -- grant does.
        DELETE FROM storage.shares s
         WHERE s.id = OLD.subject_id
           AND NOT EXISTS (
               SELECT 1 FROM storage.access_grants ag
                WHERE ag.subject_type = 'token'
                  AND ag.subject_id   = OLD.subject_id
           );
    END IF;
    RETURN OLD;
END;
$$ LANGUAGE plpgsql;

DROP TRIGGER IF EXISTS trg_cleanup_share_on_grant_delete ON storage.access_grants;
CREATE TRIGGER trg_cleanup_share_on_grant_delete
    AFTER DELETE ON storage.access_grants
    FOR EACH ROW
    EXECUTE FUNCTION storage.cleanup_share_on_last_token_grant_delete();

COMMENT ON FUNCTION storage.cleanup_share_on_last_token_grant_delete() IS
    'Reverse cascade: deletes storage.shares row when its last token grant is removed. Pairs with trg_cleanup_grants_token (forward direction).';

-- ── One-shot sweep of pre-existing orphans ─────────────────────────────────
-- Any share row that already has zero matching grants is dead weight. The
-- forward trigger never had a chance to fire on these (they ended up
-- grant-less via `DELETE /api/grants/{id}` calls predating this trigger).
DELETE FROM storage.shares s
 WHERE NOT EXISTS (
     SELECT 1 FROM storage.access_grants ag
      WHERE ag.subject_type = 'token'
        AND ag.subject_id   = s.id
 );
