# UC2.2 Dual-View Transcript Release Checklist

Use this checklist when preparing the dual-view transcription feature for release. It focuses on validating functional coverage, latency SLAs, desktop integration, and telemetry readiness so QA and release managers can certify the feature before broader rollout.

## 1. Feature Completeness
- [ ] Confirm the desktop shell exposes the dual-view panel entry point and renders both raw and polished columns without console errors. 【F:apps/desktop/src/features/transcription/DualViewPanel.tsx†L426-L535】
- [ ] Ensure the React hook hydrates transcript history, subscribes to `session://transcript`, and enforces the five-sentence selection cap (attempting a sixth should produce a guardrail announcement). 【F:apps/desktop/src/features/transcription/hooks/useDualViewTranscript.ts†L68-L110】【F:apps/desktop/src/features/transcription/hooks/useDualViewTranscript.ts†L522-L600】
- [ ] Verify revert selections dispatch through the Tauri bridge and receive acknowledgements; failure paths must raise HUD banners instead of silent drops. 【F:apps/desktop/src/features/transcription/hooks/useDualViewTranscript.ts†L522-L747】【F:apps/desktop/src/features/transcription/DualViewPanel.tsx†L426-L471】
- [ ] Exercise notice handling (latency warnings, polishing failures, cloud fallback) and confirm banners use the correct tone + screen reader messaging. 【F:apps/desktop/src/features/transcription/hooks/useDualViewTranscript.ts†L228-L304】【F:apps/desktop/src/features/transcription/DualViewPanel.tsx†L426-L471】

## 2. Accessibility and UX
- [ ] Validate keyboard traversal (arrow keys between sentence cards, toolbar focus loops) and ensure aria-labels accurately describe variant state and latency. 【F:apps/desktop/src/features/transcription/hooks/useDualViewTranscript.ts†L111-L210】【F:apps/desktop/src/features/transcription/DualViewPanel.integration.test.tsx†L73-L162】
- [ ] Confirm screen readers announce pending selections, acknowledgements, and error notices in the expected politeness channel. 【F:apps/desktop/src/features/transcription/hooks/useDualViewTranscript.ts†L522-L600】【F:apps/desktop/src/features/transcription/hooks/useDualViewTranscript.ts†L600-L747】
- [ ] Check HUD banner contrast and focus outlines against the design tokens introduced for the dual-view panel. 【F:apps/desktop/src/features/transcription/styles.css†L1-L200】

## 3. Performance Validation (SLA Gate)
- [ ] Record at least two representative transcription sessions (quiet room + noisy environment). Capture timestamps from raw and polished event payloads in telemetry logs to calculate latency.
- [ ] Confirm raw sentences flush within **400 ms** of Whisper frame availability. Inspect `latency_ms` and `within_sla` fields in `dual_view_latency` events; any breach must be justified or fixed prior to release. 【F:core/src/orchestrator/mod.rs†L327-L427】【F:core/src/telemetry/events.rs†L1-L120】
- [ ] Confirm polished sentences are delivered within **2.5 s** SLA under normal network and CPU load. Use telemetry to flag `within_sla: false` entries and ensure they stay below the agreed tolerance (≤2% of sentences) for sign-off. 【F:core/src/orchestrator/mod.rs†L1046-L1206】【F:core/src/telemetry/events.rs†L1-L120】
- [ ] Stress test with simulated CPU contention (e.g., local video encode) to verify graceful degradation: the UI must raise latency warnings while remaining responsive.

## 4. Telemetry and Logging
- [ ] Set `FLOWWISPER_TELEMETRY_DIR` to a temporary location and confirm dual-view latency and revert events serialize as JSON envelopes. 【F:core/src/telemetry/mod.rs†L12-L95】
- [ ] Validate the seven-day pruning job removes old telemetry files and does not block the orchestrator thread. 【F:core/src/telemetry/mod.rs†L96-L170】
- [ ] Ensure revert selections emit `dual_view_revert` events with matching `sentence_id`/`variant` pairs for analytics parity. 【F:core/src/telemetry/events.rs†L80-L120】【F:core/src/orchestrator/mod.rs†L1118-L1206】

## 5. Regression and Automation
- [ ] Run `cargo test` inside `core/` to cover orchestrator, session, and telemetry suites. 【F:core/src/orchestrator/mod.rs†L1214-L1339】
- [ ] Run `npm test -- --run` inside `apps/desktop/` to execute hook, component, and integration tests for the dual-view panel. 【F:apps/desktop/src/features/transcription/DualViewPanel.integration.test.tsx†L1-L372】
- [ ] Re-run smoke telemetry test `telemetry_logs_are_json_enveloped` with a temporary directory to confirm CI parity. 【F:core/src/telemetry/mod.rs†L171-L243】
- [ ] Capture manual QA notes (scenarios tested, SLA metrics, open issues) and attach them to the release ticket for traceability.

## 6. Launch Readiness
- [ ] Communicate known limitations (identity polisher, missing revert command wiring, 120-event log cap) to support and PM before toggling the feature flag. 【F:docs/onboarding/dual_view_transcript.md†L29-L53】
- [ ] Verify rollback plan: ensure feature flag or config toggle can disable the dual-view panel without redeploying the desktop shell.
- [ ] Secure sign-offs from engineering, QA, and product stakeholders before production rollout.
