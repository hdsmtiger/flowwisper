## Relevant Files

- `apps/desktop/src/App.tsx` - Current desktop shell; wire up the dual-view transcription surface and route state props.
- `apps/desktop/src/features/transcription/DualViewPanel.tsx` - New UI component to render side-by-side original vs polished sentences.
- `apps/desktop/src/features/transcription/DualViewPanel.test.tsx` - Component tests covering streaming updates, selection, and revert logic.
- `apps/desktop/src/features/transcription/DualViewPanel.integration.test.tsx` - Integration tests pairing the hook with the panel for live updates, batch selection, and accessibility shortcuts.
- `apps/desktop/src/features/transcription/hooks/useDualViewTranscript.ts` - Hook to manage sentence state, revert selections, and latency banners.
- `apps/desktop/src/features/transcription/hooks/useDualViewTranscript.test.ts` - Unit tests for hook behavior (buffering, multi-select, timers).
- `apps/desktop/src/features/transcription/styles.css` - Styling tokens for dual columns, HUD states, and accessibility focus outlines.
- `apps/desktop/src-tauri/src/session.rs` - Tauri bridge managing session lifecycle and message dispatch to the React layer.
- `apps/desktop/src-tauri/src/main.rs` - Update command wiring or event listeners for dual-stream payloads.
- `core/src/orchestrator/mod.rs` - Stream raw vs polished transcripts, enforce latency thresholds, and emit state signals.
- `core/src/session/mod.rs` - Session-level coordination for VAD sentence boundaries and revert acknowledgements.
- `core/src/telemetry/mod.rs` - Extend local logging schema with `dual_view_latency` and `dual_view_revert` events.
- `core/src/telemetry/events.rs` - (If non-existent, create) typed definitions for new telemetry events.
- `core/src/tests/dual_view.rs` - Integration tests validating latency timers and revert sync.
- `docs/sprint/sprint2.md` - Cross-check acceptance details during development.

### Notes

- Co-locate UI tests next to their components; prefer React Testing Library for interaction coverage.
- Rust integration tests can live under `core/tests/` if cross-module behavior requires end-to-end validation.
- Telemetry logging should reuse existing rolling file appender configuration; avoid introducing remote sinks per PRD.

### Progress Log

- **1.1 Raw/polished pipeline review:** `RealtimeWorker` still emits a single `TranscriptPayload` stream sourced from local or cloud engines with `TranscriptSource::Local`/`Cloud`, and degradation warnings funnel through `UpdatePayload::Notice` without any polishing branch today (see `core/src/orchestrator/mod.rs`).
- **1.2 Desktop IPC contract:** The Tauri bridge currently emits only `session://state` lifecycle updates and `audio://meter` frames, so transcript payloads will require a new channel/event payload (`apps/desktop/src-tauri/src/session.rs` and `apps/desktop/src-tauri/src/audio.rs`).
- **1.3 Latency & logging audit:** Each `TranscriptionUpdate` carries a `latency: Duration`, deadline/cadence breaches trigger `warn!`/`error!` traces, and telemetry setup is limited to initializing a plain `tracing_subscriber` without structured dual-view metrics yet (see `core/src/orchestrator/mod.rs` and `core/src/telemetry/mod.rs`).
- **2.1 Sentence boundary buffering:** Implemented a reusable `SentenceBuffer` inside the orchestrator to aggregate local deltas into sentence-level chunks or force-flush after 400 ms, with session/orchestrator tests covering punctuation and window-based emissions.
- **2.2 Polishing pipeline scaffolding:** Added an async `SentencePolisher` trait with default identity implementation, optional per-session enable flag, and orchestration hooks that emit polished transcripts, flag 2.5 s SLA breaches, and surface error notices with coverage in orchestrator tests.
- **2.3 Revert command plumbing:** Engine orchestrator now assigns sentence IDs, tracks preferred variants, processes multi-select revert commands, and emits selection acknowledgements with unit coverage.
- **2.4 Dual-view regression tests:** Expanded orchestrator unit coverage for raw-window latency metrics, cloud downgrade recovery, and multi-sentence revert acknowledgements (see new cases in `core/src/orchestrator/mod.rs`).
- **3.1 Dual-stream IPC events:** Extended the desktop session manager to emit dual-view transcript events with latency metadata, retain a rolling log, and expose a Tauri command for React subscribers.
- **3.2 Dual-view client state hook:** Added a React hook that hydrates transcript history, subscribes to live events, and manages selection/pending state for raw vs polished variants.
- **3.3 Accessibility scaffolding:** Hook now tracks focus order, builds aria labels, and raises live announcements for notices/selections to satisfy screen reader requirements.
- **4.1 Dual-column UI skeleton:** Added a DualViewPanel component with synchronized raw/polished scroll containers, focus-aware sentence cards, and shared styling tokens.
- **4.2 Multi-select UI interactions:** Implemented selection toolbar controls, five-sentence caps, and command dispatch to drive revert toggles from the React layer.
- **4.3 HUD latency + fallback UI:** Surfaced transcript notices as HUD-colored banners, added connection/error messaging, and refreshed panel layout tokens to align with the dual-view spec.
- **4.4 Automated UI tests:** Added dual-view integration tests covering streaming event renders, batch revert behavior, and arrow-key navigation accessibility.
- **5.1 Telemetry retention & event logging:** Added JSON-file telemetry sink with seven-day pruning plus raw/polished latency and revert event records from the orchestrator.
- **5.2 Telemetry log verification:** Smoke-tested the JSON sink via temp-directory logging, validated dual-view latency/revert envelopes, and captured formatting expectations for analytics.
- **5.3 Dual-view onboarding docs:** Captured the end-to-end data flow, desktop interaction checklist, telemetry hints, and current gaps (identity polisher, missing revert command wiring, 120-event log cap) in `docs/onboarding/dual_view_transcript.md` for engineers and QA.
- **5.4 Release checklist:** Documented QA, SLA validation, telemetry verification, and launch sign-off steps in `docs/onboarding/dual_view_release_checklist.md` to guide go/no-go reviews.

## Tasks

- [x] 1.0 Confirm existing transcription pipeline capabilities
  - [x] 1.1 Review current Whisper streaming + polishing flow in `core/src/orchestrator` and identify raw vs polished data hooks.
  - [x] 1.2 Validate Tauri event contract between core service and desktop shell for incremental transcript updates.
  - [x] 1.3 Document current latency metrics and logging to ensure compatibility with new telemetry events.
- [ ] 2.0 Extend core service for dual-stream outputs
  - [x] 2.1 Introduce sentence boundary detection and buffering to emit raw transcript chunks within 400ms windows.
  - [x] 2.2 Add polishing pipeline to emit AI-enhanced sentences with 2.5s SLA indicators and failure states.
  - [x] 2.3 Implement revert handling so core receives multi-select revert events and updates session state accordingly.
  - [x] 2.4 Write unit/integration tests ensuring latency timers, downgrade paths, and revert acknowledgements behave as expected.
- [ ] 3.0 Update desktop bridge and state management
  - [x] 3.1 Adjust `session.rs` and related commands to forward dual-stream payloads and latency metadata to the React layer.
  - [x] 3.2 Implement client-side state store/hook tracking sentence status, revert selections, and polished vs raw text.
  - [x] 3.3 Ensure accessibility support for keyboard focus, screen reader labels, and status announcements in state updates.
- [x] 4.0 Build dual-view UI and interactions
  - [x] 4.1 Create reusable dual-column component with synchronized scrolling and sentence cards.
  - [x] 4.2 Implement multi-select UI with batch revert action, plus per-sentence toggle control.
  - [x] 4.3 Surface latency banners, error fallback messaging, and style tokens aligning with HUD spec.
  - [x] 4.4 Add automated UI tests covering streaming updates, multi-select flows, and accessibility shortcuts.
- [x] 5.0 Logging, QA, and rollout readiness
  - [x] 5.1 Extend telemetry schema/files to record latency metrics and revert events locally with 7-day retention.
  - [x] 5.2 Verify logs through manual smoke tests and adjust formatting for analytics consumption.
  - [x] 5.3 Update documentation/onboarding materials outlining dual-view behavior and known limitations.
  - [x] 5.4 Prepare release checklist including performance validation against SLA (400ms raw, 2.5s polished).
