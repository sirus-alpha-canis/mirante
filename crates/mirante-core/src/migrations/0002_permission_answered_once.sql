-- A permission request is answered exactly once: once it leaves 'pending' it is frozen.
CREATE TRIGGER permission_requests_answered_once BEFORE UPDATE ON permission_requests
WHEN OLD.status != 'pending'
BEGIN SELECT RAISE(ABORT, 'permission request already answered'); END;
