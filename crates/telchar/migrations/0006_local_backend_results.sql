CREATE TABLE local_backend_execution_results (
    backend_execution_id text PRIMARY KEY REFERENCES local_backend_executions(backend_execution_id) ON DELETE RESTRICT,
    classification text NOT NULL CONSTRAINT local_backend_execution_results_classification_check CHECK (
        classification IN (
            'succeeded',
            'build-failure',
            'infrastructure-failure',
            'admission-failure',
            'input-failure',
            'output-failure',
            'cancelled',
            'internal-failure'
        )
    ),
    result_metadata jsonb NOT NULL CONSTRAINT local_backend_execution_results_metadata_check CHECK (
        jsonb_typeof(result_metadata) = 'object'
        AND octet_length(result_metadata::text) <= 1048576
    ),
    created_at timestamptz NOT NULL DEFAULT now()
);
