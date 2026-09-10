-- Persists the execution trace context for each durable backend attempt.
ALTER TABLE shared_build_attempts
    ADD COLUMN traceparent text CONSTRAINT shared_build_attempts_traceparent_check CHECK (
        traceparent IS NULL OR length(traceparent) BETWEEN 1 AND 55
    ),
    ADD COLUMN tracestate text CONSTRAINT shared_build_attempts_tracestate_check CHECK (
        tracestate IS NULL OR length(tracestate) BETWEEN 1 AND 512
    ),
    ADD CONSTRAINT shared_build_attempts_trace_context_check CHECK (
        traceparent IS NOT NULL OR tracestate IS NULL
    );
