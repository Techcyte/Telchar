ALTER TABLE attempt_log_archives ADD COLUMN spool_name text CHECK (
    octet_length(spool_name) BETWEEN 1 AND 256 AND spool_name !~ '[/\\]'
);
