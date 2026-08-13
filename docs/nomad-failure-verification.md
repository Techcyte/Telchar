# Nomad failure verification

Telchar treats one Nomad job as one durable shared-build attempt. Failure does not create another allocation or select another compatible backend. A later independent request may create replacement work after the failed shared build reaches terminal state.

## Terminal authority

The requester-side shared-build leader converts backend submission, monitoring, cancellation, and timeout errors into one durable `backend-failure`. The callback service converts authenticated build failure into `nomad-build-failure` and output collection or transfer failure into `nomad-transfer-failure`. Durable completion is idempotent, so concurrent requester, callback, and recovery paths cannot create a second terminal outcome.

The focused evidence is:

| Failure surface | Production path | Focused executable evidence |
| --- | --- | --- |
| optional prestart failure or timeout | Nomad prevents the build task from starting; allocation becomes failed; exact job monitoring returns backend failure | `renders_operator_selected_driver_and_stable_backend_bound_job`, `maps_allocation_terminal_states_and_missing_jobs` |
| authentication and replay | callback admission rejects before transfer; Nomad attempt later fails or times out through exact job monitoring | `rejects_foreign_claims_and_unsupported_algorithm`, `verifies_exact_hmac_callback_and_rejects_replay`, `purges_expired_nonces_and_fails_closed_at_capacity`, callback-admission rejection tests |
| manifest authority | callback transfer rejects malformed, oversized, or inconsistent admitted specifications | `rejects_manifest_when_exact_build_specification_disagrees`, `rejects_invalid_manifest_and_path_metadata`, `rejects_oversized_metadata_and_payload_before_allocation` |
| allocation-local store and substitution resolution | worker failure exits the build task; Nomad reports the allocation failure | worker environment and exact manifest tests, plus `maps_allocation_terminal_states_and_missing_jobs` |
| input transfer | callback or worker rejects foreign, duplicate, unrequested, mismatched, out-of-order, or oversized input data | `rejects_foreign_duplicate_unrequested_and_mismatched_inputs`, `enforces_aggregate_input_limit_before_requesting_transfer`, transfer protocol/session tests |
| pending placement | exact job remains monitored until its execution timeout; Telchar does not resubmit | `timeout_stops_only_the_exact_submitted_nomad_job` |
| allocation and task failure | any failed allocation produces backend failure; foreign or terminal callback allocation identity is rejected | `maps_allocation_terminal_states_and_missing_jobs`, `rejects_foreign_or_terminal_callback_allocation_identity` |
| log transfer | sequence gaps, payload misuse, oversized records, and connection interruption fail the callback transfer | `rejects_log_sequence_gaps_payload_misuse_and_early_success`, protocol payload-bound tests, callback shutdown test |
| output transfer and collection | missing, foreign, duplicate, oversized, out-of-order, rejected, corrupt, or incomplete outputs fail before durable success | `rejects_foreign_duplicate_oversized_and_out_of_order_outputs`, `completes_only_after_every_exact_output_is_received_and_accepted`, transfer session tests |
| missing or foreign job | exact monitoring returns missing or rejects mismatched backend/system metadata | `maps_allocation_terminal_states_and_missing_jobs`, `rejects_foreign_job_at_deterministic_identity` |
| explicit cancellation | DELETE targets only the deterministic persisted job with exact namespace and purge authority | `cancellation_stops_only_the_exact_submitted_nomad_job` |
| execution timeout | the exact deterministic job is purged, then the attempt returns `TimedOut` and becomes one durable backend failure | `timeout_stops_only_the_exact_submitted_nomad_job`, shared-build backend-failure persistence tests |
| restart and transfer recovery | recovery trusts exact gateway outputs first; otherwise it adopts only the original backend and persisted execution identity, never resubmitting | `complete_expected_outputs_win_before_backend_recovery`, `attempt_execution_identity_disagreement_fails_closed`, `capability_disagreement_and_missing_adopted_execution_fail_closed`, `configured_backend_adopts_exact_nomad_execution` |

## Invariants

- Allocation state `complete` does not imply build success. Exact callback-authoritative outputs must already be durable and present in the gateway store.
- Callback rejection does not cancel or replace a shared execution by itself. Exact job monitoring owns the attempt until allocation failure or the execution deadline.
- Cancellation and timeout purge only the persisted deterministic job in its configured namespace.
- Transport recovery may repeat a verified object transfer. It does not repeat `BuildDerivation` or submit another Nomad job.
- Requester disconnect never cancels the shared leader.
- Telchar performs no automatic retry and does not migrate in-flight work between compatible backends.
