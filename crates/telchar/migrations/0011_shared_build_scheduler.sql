CREATE TABLE shared_build_scheduler_state (
    singleton boolean PRIMARY KEY DEFAULT true CONSTRAINT shared_build_scheduler_state_singleton_check CHECK (singleton),
    last_admitted_subject text CONSTRAINT shared_build_scheduler_state_subject_check CHECK (
        last_admitted_subject IS NULL OR length(last_admitted_subject) BETWEEN 1 AND 4096
    ),
    updated_at timestamptz NOT NULL DEFAULT now()
);

INSERT INTO shared_build_scheduler_state (singleton, last_admitted_subject)
VALUES (true, NULL);
