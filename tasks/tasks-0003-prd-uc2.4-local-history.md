## Relevant Files

- `core/Cargo.toml` - Declare SQLCipher-capable SQLite dependency, feature flags, and build scripts for the core daemon.
- `core/build.rs` - Ensure SQLCipher environment variables/feature toggles are wired for multi-platform builds.
- `core/src/main.rs` - Register background tasks and initialize persistence services during daemon bootstrap.
- `core/src/orchestrator/mod.rs` - Schedule TTL cleanup and wire session lifecycle hooks to persistence commands.
- `core/src/persistence/mod.rs` - Implement SQLCipher storage, FTS5 search, migrations, retries, and telemetry hooks.
- `core/src/persistence/sqlite.rs` - (New) Encapsulate pooled SQLCipher connections, schema migrations, and query helpers.
- `core/src/persistence/tests.rs` - (New) Integration tests covering inserts, search, TTL cleanup, and accuracy flag updates.
- `core/src/session/mod.rs` - Emit persistence requests when sessions complete, expose APIs for history actions, and handle clipboard fallbacks.
- `core/src/session/history.rs` - (New) Define domain models / DTOs for history search, accuracy flags, and telemetry payloads.
- `core/src/telemetry/events.rs` - Add events for history persistence, accuracy feedback, and cleanup diagnostics.
- `apps/desktop/src-tauri/src/main.rs` - Register Tauri commands for querying history, marking accuracy, and exporting actions.
- `apps/desktop/src-tauri/src/session.rs` - Surface clipboard fallback, new notices, and bind UI consumers to history results.
- `apps/desktop/src/lib/history.ts` - (New) Client-side fetcher/utilities for history APIs and offline caching helpers.
- `apps/desktop/src/lib/history.test.ts` - Unit tests for the history client utilities.
- `docs/architecture.md` - Update persistence diagram and key management notes to reflect implemented flow.
- `docs/sprint/sprint2.md` - Cross-link the delivery plan and acceptance checks for UC2.4.
- `docs/onboarding/local_history_runbook.md` - Runbook covering SQLCipher key rotation, DB reset, TTL verification, and troubleshooting。

### Notes

- Favor a dedicated async task runner (Tokio interval or orchestrator queue) for TTL cleanup to avoid blocking audio threads.
- Mock SQLCipher using in-memory SQLite during tests but guard feature flags to ensure FTS5 queries compile.
- Telemetry events should include session IDs and error codes to trace dropouts per success metrics in the PRD.
- Keep cross-platform key handling behind an abstraction to support both Secure Enclave and DPAPI/TPM flows.

## Tasks

- [x] 1.0 Bootstrap SQLCipher persistence infrastructure
  - [x] 1.1 Add SQLCipher-capable SQLite dependency, build flags, and verify compilation on macOS/Windows targets.
  - [x] 1.2 Implement connection manager that unwraps encryption keys via existing key management utilities.
  - [x] 1.3 Create migrations for `sessions` table, JSON columns, and FTS5 index with multi-language tokenizers.
  - [x] 1.4 Write initialization tests ensuring schema bootstraps, encryption is enforced, and migrations run idempotently.

- [x] 2.0 Persist completed sessions with retry and fallback logic
  - [x] 2.1 Extend session lifecycle to queue persistence when a session transitions from `Publishing` to `Completed`.
  - [x] 2.2 Encode asynchronous write job with 200 ms SLA, three retry attempts, and telemetry emission on failure.
  - [x] 2.3 Capture clipboard backup + user notice when persistence fails after retries.
  - [x] 2.4 Add unit tests / mocks verifying persistence commands are enqueued, retries respected, and telemetry fired.

- [x] 3.0 Deliver history retrieval and search interfaces
  - [x] 3.1 Implement persistence queries supporting keyword search, locale/app filters, and paginated previews.
  - [x] 3.2 Expose domain DTOs and Tauri commands (or IPC endpoints) returning combined metadata and transcripts.
  - [x] 3.3 Build desktop client utilities to call the commands, surface previews, and handle offline caches.
  - [x] 3.4 Add integration tests covering FTS search accuracy, pagination, and serialization to the UI layer.

- [x] 4.0 Capture accuracy feedback and telemetry synchronization
  - [x] 4.1 Extend schema to store accuracy flags, remarks, and post-actions per session record.
  - [x] 4.2 Provide APIs for marking accuracy, undoing flags, and re-inserting transcripts.
  - [x] 4.3 Queue telemetry payloads for feedback events and ensure background sync respects offline mode.
  - [x] 4.4 Write tests validating state transitions, telemetry queuing, and undo flows.

- [x] 5.0 Enforce 48-hour retention and diagnostics
  - [x] 5.1 Schedule periodic TTL cleanup job within orchestrator, ensuring index + blob cleanup stays asynchronous.
  - [x] 5.2 Log cleanup metrics, surface telemetry events, and expose diagnostics for desktop UI/tooling.
  - [x] 5.3 Add tests simulating expired records to confirm deletion, logging, and error retries.

- [x] 6.0 Documentation and release readiness
  - [x] 6.1 Update architecture and sprint docs to reflect delivered persistence flow and acceptance criteria.
  - [x] 6.2 Document runbooks for key rotation, DB reset, and troubleshooting in README or ops notes.
  - [x] 6.3 Prepare QA checklist covering acceptance metrics (write latency, search SLA, TTL coverage).
