CREATE INDEX protocol_sessions_closed_history_idx
    ON protocol_sessions (session_id) WHERE state = 'closed';
