ALTER TABLE attempt_log_archives DROP CONSTRAINT attempt_log_archives_state_check;
ALTER TABLE attempt_log_archives ADD CHECK (state IN ('capturing', 'pending', 'available', 'failed', 'expired'));
