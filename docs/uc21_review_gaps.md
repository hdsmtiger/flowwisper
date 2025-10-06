# UC2.1 Review Gaps and Remediation Plan

This document tracks the outstanding gaps identified by the senior architect for UC2.1 and records their remediation status and validation steps.

## Gap Tracker

1. **RealtimeWorker dual-path orchestration** — *Status: Completed*
   - Issue: With `prefer_cloud = false`, the realtime worker does not start the cloud engine and incorrectly marks local results as non-primary, violating the "local-first, cloud fallback" requirement.
   - Desired fix: Always start both local and cloud workers, treat local output as primary when `prefer_cloud = false`, and ensure cloud fallback notices are emitted on failures without blocking the first local update.

2. **SessionManager pipeline integration** — *Status: Completed*
   - Issue: Session manager defaults to `prefer_cloud = true` and does not wire the audio pipeline's 100–200 ms frame flow or session state broadcasts into the realtime transcription channel, preventing the UC2.1 pacing and UI feedback loop.
   - Desired fix: Default to local-first orchestration, connect audio frame scheduling and session state notifications to the realtime session handle, and ensure incremental updates follow the specified cadence.

3. **WhisperLocalEngine streaming state** — *Status: Completed*
   - Issue: The whisper-based local engine recreates decoder state per frame and runs full decoding, so it cannot maintain incremental context or meet the 400 ms first-character target.
   - Desired fix: Maintain persistent decoder state across frames, reuse context for streaming increments, or otherwise introduce minimal caching to deliver true incremental decoding behaviour.

4. **Per-source first-update enforcement** — *Status: Completed*
   - Issue: A fast cloud diff could satisfy the 400 ms watchdog without proving the local decoder met the UC2.1 latency goal, so local misses were invisible.
   - Desired fix: Track local completion separately, trigger deadline notices when the Whisper path misses 400 ms even if cloud results arrive, and keep local results marked as primary in local-first mode.

5. **Cloud retry and recovery path** — *Status: Completed*
   - Issue: After the first cloud failure the orchestrator permanently disabled the cloud decoder, preventing long-session fallback resilience.
   - Desired fix: Introduce a backoff circuit that retries after transient errors, restores cloud primacy when healthy, and continues emitting fallback notices on subsequent failures.

6. **Whisper windowed decoding** — *Status: Completed*
   - Issue: The Whisper wrapper re-decoded the full PCM history on each frame, making latency grow with session length and violating the 200 ms cadence target.
   - Desired fix: Retain streaming state with a bounded rolling window, avoid unbounded history growth, and compute incremental diffs without replaying the entire session.

7. **Sustained local-first gating** — *Status: Completed*
   - Issue: `local_update_flag` never resets after the first Whisper diff, so later cloud tasks skip the local wait and can pre-empt Whisper output, breaking the "本地优先、云端回退" mandate for UC2.1.【F:docs/sprint/sprint2.md†L8-L11】【F:docs/architecture.md†L92-L114】【F:docs/architecture.md†L187-L201】
   - Resolution: Introduced `LocalProgress` tracking, cloud-side gating with per-frame deadlines, and degradation-aware notifications so the fallback path only opens when Whisper actually stalls or fails.【F:core/src/orchestrator/mod.rs†L90-L189】【F:core/src/orchestrator/mod.rs†L324-L412】【F:core/src/orchestrator/mod.rs†L520-L606】

8. **Rolling cadence watchdog** — *Status: Completed*
   - Issue: The deadline monitor only checked the very first update and all later transcripts reused the session start timestamp, leaving multi-second local stalls undetected despite the 200 ms cadence requirement.【F:docs/sprint/sprint2.md†L8-L11】【F:docs/architecture.md†L187-L201】
   - Resolution: Added a rolling watchdog that re-arms on every cadence window, emits incremental notices, and reports per-frame latencies based on frame start instants.【F:core/src/orchestrator/mod.rs†L90-L189】【F:core/src/orchestrator/mod.rs†L561-L606】

9. **Silence-triggered first-window fallbacks** — *Status: Completed*
   - Issue: The realtime watchdog emits the 400 ms fallback notice whenever the local decoder has not produced text, even during legitimate silent frames, so the UI sees false degradation warnings at session start.【F:core/src/orchestrator/mod.rs†L60-L189】
   - Resolution: `RealtimeSession` now records RMS-based speech activity, arms the first-window deadline only after voice is detected, and measures latency from that speech onset so silence never triggers fallback prompts.【F:core/src/orchestrator/mod.rs†L60-L238】

10. **Whisper streaming still replays lookback windows** — *Status: Completed*
    - Issue: `WhisperLocalEngine::transcribe` continues to call `WhisperState::full` on every frame, re-decoding the entire lookback slice and exceeding the 35–45 ms budget once the buffer fills, which undermines UC2.1’s 200 ms cadence target.【F:core/src/orchestrator/mod.rs†L777-L940】
    - Resolution: The streaming decoder maintains a 240 ms tail plus pending audio, flushing when speech energy or stride thresholds are met so each decode covers ≤ ~450 ms instead of replaying the full session history.【F:core/src/orchestrator/mod.rs†L777-L940】

11. **PCM frame segmentation is unenforced** — *Status: Completed*
    - Issue: Frames forwarded through `SessionManager::start_realtime_transcription` bypass the duration checks in `RealtimeSessionHandle::push_frame`, and the audio pipeline forwards arbitrary byte lengths, so the orchestrator may receive buffers outside the mandated 100–200 ms window without detection.【F:core/src/session/mod.rs†L49-L88】【F:core/src/audio/mod.rs†L1-L155】
    - Resolution: `AudioPipeline::push_pcm_frame` buffers incoming PCM, rechunks it into 100–200 ms slices with RMS/VAD metadata, and dispatches over bounded queues so downstream consumers always receive cadence-compliant frames.【F:core/src/audio/mod.rs†L1-L155】

12. **Rolling cadence watchdog misfires during silence** — *Status: Completed*
    - Issue: The realtime monitor only tracks the timestamp of the last successful local transcript and triggers cadence violations whenever no new text arrives for a cadence window, even when no speech is present. `LocalProgress` never records silence periods, so the watchdog keeps emitting "本地解码增量延迟" notices during natural pauses.【F:core/src/orchestrator/mod.rs†L90-L198】【F:core/src/orchestrator/mod.rs†L304-L358】
    - Resolution: `LocalProgress` now records per-frame RMS and speech activity so the rolling watchdog pauses during silence and only re-arms once speech resumes, preventing false degradation notices.【F:core/src/orchestrator/mod.rs†L90-L210】【F:core/src/orchestrator/mod.rs†L304-L372】

13. **Audio fan-out stalls realtime pipeline** — *Status: Completed*
   - Issue: `AudioPipeline::emit_chunk` awaits each subscriber send sequentially, so a slow auxiliary consumer can block the orchestrator feed and violate the 100–200 ms delivery cadence required for UC2.1.【F:core/src/audio/mod.rs†L99-L120】
   - Resolution: `AudioPipeline::emit_chunk` now tries to send without awaiting and offloads back-pressured subscribers to background tasks, preserving the orchestrator’s 100–200 ms cadence while still delivering frames to ancillary listeners.【F:core/src/audio/mod.rs†L99-L161】

14. **Cloud-first preempts Whisper-first deadline** — *Status: Completed*
   - Issue: When `prefer_cloud = true`, the cloud worker bypasses the local gating branch and is allowed to publish the very first transcript as soon as it finishes, so a faster cloud response still satisfies the watchdog without the Whisper path meeting UC2.1’s “本地引擎在 400ms 内先出首字” requirement.【F:docs/sprint/sprint2.md†L8-L11】【F:docs/architecture.md†L28-L33】
   - Resolution: Cloud workers now gate on the Whisper-first flag even in cloud-preferred sessions, only releasing transcripts after the local engine succeeds, degrades, or times out, and a regression verifies that a faster cloud response still leaves the first update sourced from Whisper.【F:core/src/orchestrator/mod.rs†L719-L787】【F:core/src/orchestrator/mod.rs†L1342-L1385】

15. **PCM accumulator flush is missing** — *Status: Completed*
   - Issue: `AudioPipeline::push_pcm_frame` only emits chunks while the buffer has at least 100 ms of PCM and leaves any remainder in `pending`, so the final <100 ms of captured speech never reaches the realtime session, violating the architecture’s 100–200 ms streaming contract.【F:docs/architecture.md†L96-L103】
   - Resolution: Added `AudioPipeline::flush_pending` and invoked it from the session manager to pad and deliver any trailing samples so Whisper receives the full recording tail for each session.【F:core/src/audio/mod.rs†L73-L120】【F:core/src/session/mod.rs†L57-L84】

16. **PCM fan-out ordering is nondeterministic** — *Status: Completed*
   - Issue: `AudioPipeline::emit_chunk` now spawns detached `send` tasks whenever a subscriber channel is full. Multiple in-flight tasks race to acquire capacity on the same channel, allowing later frames to overtake earlier ones and breaking UC2.1’s requirement that 100–200 ms slices reach Whisper in order.【F:docs/sprint/sprint2.md†L8-L11】【F:docs/architecture.md†L96-L114】【F:core/src/audio/mod.rs†L138-L173】
   - Desired fix: Serialize delivery per subscriber (e.g., a dedicated forwarder task or bounded queue) so backpressure never reorders frames, or explicitly drop/notify while keeping the published sequence deterministic.

17. **Cloud gating ignores local send backpressure** — *Status: Completed*
   - Issue: The local worker records success and notifies the cloud gate before awaiting the `tx.send` call, so under a saturated updates channel the cloud branch can run once `last_frame` advances and deliver text ahead of the still-pending Whisper diff, violating the 400 ms “Whisper first” commitment for UC2.1.【F:docs/sprint/sprint2.md†L8-L11】【F:core/src/orchestrator/mod.rs†L642-L760】
   - Resolution: `spawn_local_task` now keeps the Whisper-first claim and `LocalProgress` advancement gated until `tx.send` succeeds, rolls back the claim when delivery fails, and only then notifies the cloud gate so backpressure can no longer let the cloud path preempt local delivery.【F:core/src/orchestrator/mod.rs†L632-L709】

18. **PCM subscriber queues are unbounded** — *Status: Completed*
   - Issue: `PcmSubscriber::enqueue` stores every frame in an unbounded `VecDeque`, so a stalled consumer retains the entire session in memory despite the architecture’s requirement to cap in-memory audio buffers (~3 minutes/<80 MB).【F:docs/architecture.md†L187-L202】【F:core/src/audio/mod.rs†L33-L89】
   - Resolution: Each `PcmSubscriber` enforces a bounded queue sized to a small multiple of its channel capacity, drops and logs the oldest frame when the limit is exceeded, and a regression test ensures only the most recent frames survive prolonged backpressure.【F:core/src/audio/mod.rs†L26-L214】【F:core/src/audio/mod.rs†L288-L370】

19. **Cloud gating collapses after the first Whisper diff** — *Status: Completed*
   - Resolution: Cloud workers now consult `LocalProgress` before each decode and remain gated until the matching local frame succeeds, degrades, or times out, keeping Whisper ahead across the full session regardless of `prefer_cloud`. Regression coverage spans both local-first and cloud-preferred strategies.【F:core/src/orchestrator/mod.rs†L713-L744】【F:core/src/orchestrator/mod.rs†L1338-L1374】【F:core/src/orchestrator/mod.rs†L1460-L1519】

20. **Cloud fallback never becomes primary after local failure** — *Status: Completed*
   - Resolution: When the local path degrades, cloud transcripts are promoted to primary so downstream consumers receive the fallback content until Whisper recovers, with regression coverage confirming the promotion behaviour.【F:core/src/orchestrator/mod.rs†L688-L736】【F:core/src/orchestrator/mod.rs†L1266-L1303】

21. **Fallback transcripts stay secondary on latency misses** — *Status: Completed*
   - Issue: The watchdog only emits notices when Whisper exceeds the 400 ms/200 ms targets but never marks the session degraded, so `local_progress.is_degraded()` remains `false` and cloud transcripts continue to ship with `is_primary = false`, leaving the UI without a promoted fallback during SLA violations.【F:core/src/orchestrator/mod.rs†L120-L200】【F:core/src/orchestrator/mod.rs†L790-L799】【F:docs/sprint/sprint2.md†L8-L11】
   - Desired fix: Propagate the watchdog outcome into `LocalProgress` (or equivalent) so the cloud path can flip `is_primary` and downstream consumers pivot to the fallback transcript whenever the local engine misses the UC2.1 latency budget.

22. **PCM shedding drops primary audio under backpressure** — *Status: Completed*
   - Issue: `PcmSubscriber::enqueue` pops the oldest frame once its queue reaches `max_queue`, meaning the primary Whisper feed silently loses 100–200 ms PCM slices whenever the consumer stalls, contradicting UC2.1’s requirement that every frame reach the decoder.【F:core/src/audio/mod.rs†L55-L99】【F:docs/sprint/sprint2.md†L8-L11】【F:docs/architecture.md†L96-L103】
   - Desired fix: Keep the orchestrator feed lossless (e.g., by letting the session apply backpressure or isolating drops to non-critical subscribers) and trigger degradation handling instead of discarding audio.

23. **Local progress regresses on out-of-order completions** — *Status: Open*
   - Issue: `LocalProgress::record_success` overwrites `last_frame` with the incoming frame index using a plain `store`, so when a slower earlier frame finally delivers it rewinds the recorded progress, confusing the cadence watchdog and cloud gate even though later frames already succeeded, which violates UC2.1’s 200 ms incremental target.【F:core/src/orchestrator/mod.rs†L327-L333】【F:docs/sprint/sprint2.md†L8-L11】
   - Desired fix: Update `last_frame` monotonically (e.g., `fetch_max`) or track contiguous completion so late results cannot move the pointer backwards before upstream ordering has caught up.

24. **Local transcripts can publish out of order** — *Status: Open*
   - Issue: Every audio frame spawns a detached Tokio task with no sequencing, allowing later frames to finish first, publish their transcripts, and release the cloud gate ahead of earlier local results, breaking the architecture’s "本地优先" stream ordering requirement.【F:core/src/orchestrator/mod.rs†L605-L719】【F:docs/architecture.md†L92-L103】【F:docs/architecture.md†L187-L201】
   - Desired fix: Serialize local decoding or add per-frame acknowledgement tracking so the orchestrator only advances once preceding frames have been committed, keeping fallback results strictly behind Whisper output.

## Validation Log

### RealtimeWorker dual-path orchestration
- **Status:** Completed
- **Validation:** `cargo test --manifest-path core/Cargo.toml`
- **Notes:** Ensured cloud fallback tasks spawn alongside local-primary decoding and verified fallback notices for both cloud-preferred and local-preferred strategies.

### SessionManager pipeline integration
- **Status:** Completed
- **Validation:** `cargo test --manifest-path core/Cargo.toml`
- **Notes:** Defaulted orchestration to local-first, bridged audio PCM broadcasts into the realtime session frame queue, and relayed updates to the session broadcast bus with unit coverage for end-to-end propagation.

### WhisperLocalEngine streaming state
- **Status:** Completed
- **Validation:** `cargo test --manifest-path core/Cargo.toml`
- **Notes:** Persisted whisper decoder state across frames with bounded PCM history, incremental diffing, and mutex-guarded access to honour UC2.1’s 400 ms streaming latency expectations.

### Per-source first-update enforcement
- **Status:** Completed
- **Validation:** `cargo test --manifest-path core/Cargo.toml`
- **Notes:** Added a dedicated local decoder deadline flag, ensured cloud-first updates still trigger late-local notices, gated cloud emissions on the Whisper notifier, and covered the scenario with `emits_deadline_notice_when_local_is_late`.

### Cloud retry and recovery path
- **Status:** Completed
- **Validation:** `cargo test --manifest-path core/Cargo.toml`
- **Notes:** Implemented a cloud backoff circuit with automatic retries and a regression test (`retries_cloud_after_backoff`) that exercises recovery after a transient outage.

### Whisper windowed decoding
- **Status:** Completed
- **Validation:** `cargo test --manifest-path core/Cargo.toml`
- **Notes:** Limited whisper reprocessing to a rolling tail with a 400 ms lookback window, added overlap-aware diffing without replaying the full session, and verified latency protections alongside the updated orchestrator suite.

### Sustained local-first gating
- **Status:** Completed
- **Validation:** `cargo test --manifest-path core/Cargo.toml`【8924ea†L1-L4】
- **Notes:** `LocalProgress`-based gating and the `cloud_waits_for_local_each_frame` regression ensure cloud results never pre-empt Whisper output unless the session enters a degraded fallback state.【F:core/src/orchestrator/mod.rs†L90-L189】【F:core/src/orchestrator/mod.rs†L520-L606】【F:core/src/orchestrator/mod.rs†L1099-L1155】

### Rolling cadence watchdog
- **Status:** Completed
- **Validation:** `cargo test --manifest-path core/Cargo.toml`【8924ea†L1-L4】
- **Notes:** The re-armed watchdog now surfaces late-local cadence slips beyond the first diff, with notices verified through the existing late-local orchestration test suite.【F:core/src/orchestrator/mod.rs†L90-L189】【F:core/src/orchestrator/mod.rs†L1009-L1098】

### Silence-triggered first-window fallbacks
- **Status:** Completed
- **Validation:** `cargo test --manifest-path core/Cargo.toml`
- **Notes:** Added RMS-based speech detection and a 50 ms polling loop so the first-window watchdog arms only after voice is present and measures latency from that speech onset.【F:core/src/orchestrator/mod.rs†L60-L238】

### Whisper streaming still replays lookback windows
- **Status:** Completed
- **Validation:** `cargo test --manifest-path core/Cargo.toml`
- **Notes:** Whisper streaming now maintains a 240 ms tail plus pending audio and clears processed buffers each decode so only ≤ ~450 ms of PCM is reprocessed per increment.【F:core/src/orchestrator/mod.rs†L777-L940】

### PCM frame segmentation is unenforced
- **Status:** Completed
- **Validation:** `cargo test --manifest-path core/Cargo.toml`
- **Notes:** The audio pipeline now buffers PCM, rechunks into 100–200 ms slices, broadcasts VAD metrics, and fans out via bounded queues so downstream consumers only receive cadence-compliant frames.【F:core/src/audio/mod.rs†L1-L155】

### Rolling cadence watchdog misfires during silence
- **Status:** Completed
- **Validation:** `cargo test --manifest-path core/Cargo.toml`
- **Notes:** RMS-aware `LocalProgress` tracking keeps the cadence watchdog armed only while speech is active, with coverage from the `silence_does_not_trigger_cadence_notice` regression test.【F:core/src/orchestrator/mod.rs†L90-L210】【F:core/src/orchestrator/mod.rs†L304-L372】

### Audio fan-out stalls realtime pipeline
- **Status:** Completed
- **Validation:** `cargo test --manifest-path core/Cargo.toml`
- **Notes:** Non-blocking fan-out with background delivery tasks ensures slow subscribers cannot stall realtime ingestion, proven by `slow_subscriber_does_not_block_realtime_feed`.【F:core/src/audio/mod.rs†L99-L161】

### Cloud-first preempts Whisper-first deadline
- **Status:** Completed
- **Validation:** `cargo test --manifest-path core/Cargo.toml`
- **Notes:** Cloud updates now wait on the Whisper-first notifier even in cloud-preferred sessions, with `cloud_preferred_sessions_emit_local_first` ensuring local transcripts still arrive first when the cloud engine is faster.【F:core/src/orchestrator/mod.rs†L719-L787】【F:core/src/orchestrator/mod.rs†L1342-L1385】

### PCM accumulator flush is missing
- **Status:** Completed
- **Validation:** `cargo test --manifest-path core/Cargo.toml`
- **Notes:** `AudioPipeline::flush_pending` pads trailing audio and the session manager flushes it on shutdown, covered by `flushes_pending_tail_on_request`.【F:core/src/audio/mod.rs†L73-L120】【F:core/src/session/mod.rs†L57-L84】

### PCM fan-out ordering is nondeterministic
- **Status:** Completed
- **Validation:** `cargo test --manifest-path core/Cargo.toml`
- **Notes:** Added buffered per-subscriber forwarders that stream frames through a serialized worker so even when downstream queues fill, delivery remains FIFO while `slow_subscriber_does_not_block_realtime_feed` and `preserves_order_under_backpressure` cover both non-blocking ingestion and ordering guarantees.【F:core/src/audio/mod.rs†L32-L214】

### Cloud gating ignores local send backpressure
- **Status:** Completed
- **Validation:** `cargo test --manifest-path core/Cargo.toml`
- **Notes:** Regression coverage around `cloud_waits_for_local_each_frame` confirms the cloud path remains gated until the Whisper diff is delivered, with `spawn_local_task` delaying notifier updates until `tx.send` succeeds even under saturated channels.【F:core/src/orchestrator/mod.rs†L632-L709】【F:core/src/orchestrator/mod.rs†L1110-L1197】

### PCM subscriber queues are unbounded
- **Status:** Completed
- **Validation:** `cargo test --manifest-path core/Cargo.toml`
- **Notes:** Bounded per-subscriber queues sized to 4× channel capacity drop and log the oldest frames under sustained backlog, and `drops_oldest_frame_when_queue_is_full` verifies monotonic delivery while shedding stale audio to protect the buffering budget.【F:core/src/audio/mod.rs†L26-L214】【F:core/src/audio/mod.rs†L288-L370】

### Cloud gating collapses after the first Whisper diff
- **Status:** Completed
- **Validation:** `cargo test --manifest-path core/Cargo.toml`
- **Notes:** Cloud workers now loop on `LocalProgress` for every frame, keeping Whisper ahead during normal operation while allowing cloud results through only after local success, degradation, or timeout. The `cloud_waits_for_local_each_frame` and `cloud_preferred_waits_for_whisper_each_frame` regressions exercise both local-first and cloud-preferred modes.【F:core/src/orchestrator/mod.rs†L713-L744】【F:core/src/orchestrator/mod.rs†L1338-L1374】【F:core/src/orchestrator/mod.rs†L1460-L1519】

### Cloud fallback never becomes primary after local failure
- **Status:** Completed
- **Validation:** `cargo test --manifest-path core/Cargo.toml`
- **Notes:** When Whisper degrades the orchestrator promotes the cloud transcript (`is_primary = true`) and the fallback regression confirms the promotion so downstream consumers pivot to the cloud path during outages.【F:core/src/orchestrator/mod.rs†L688-L736】【F:core/src/orchestrator/mod.rs†L1266-L1303】

### Fallback transcripts stay secondary on latency misses
- **Status:** Completed
- **Validation:** `cargo test --manifest-path core/Cargo.toml`
- **Notes:** The realtime watchdog now marks `LocalProgress` degraded whenever the 400 ms first-window or rolling cadence timers
  fire, and the cloud path promotes fallback transcripts while that degraded flag is set, ensuring the UI pivots to the fallback
  stream whenever Whisper breaches the SLA.【F:core/src/orchestrator/mod.rs†L120-L207】【F:core/src/orchestrator/mod.rs†L688-L736】【F:docs/sprint/sprint2.md†L8-L11】

### PCM shedding drops primary audio under backpressure
- **Status:** Completed
- **Validation:** `cargo test --manifest-path core/Cargo.toml`
- **Notes:** `AudioPipeline` exposes a lossless subscription option that blocks on backpressure instead of shedding frames, and
  the session manager now uses that path for the primary Whisper feed so every 100–200 ms slice is delivered while auxiliary
  subscribers retain bounded, shedding queues to protect the realtime cadence.【F:core/src/audio/mod.rs†L23-L170】【F:core/src/session/mod.rs†L60-L96】【F:docs/architecture.md†L96-L103】

### Local progress regresses on out-of-order completions
- **Status:** Completed
- **Validation:** `cargo test --manifest-path core/Cargo.toml`
- **Notes:** `LocalProgress::record_success` now advances `last_frame` via a compare-and-swap loop so late Whisper completions cannot rewind progress, keeping the cadence watchdog aligned with delivered frames.【F:core/src/orchestrator/mod.rs†L329-L350】

### Local transcripts can publish out of order
- **Status:** Completed
- **Validation:** `cargo test --manifest-path core/Cargo.toml`
- **Notes:** Local transcription tasks serialize through a mutex-guarded critical section, ensuring Whisper emits 100–200 ms increments in order before the cloud fallback is released.【F:core/src/orchestrator/mod.rs†L567-L705】

### Cloud promotion still overrides Whisper while healthy
- **Status:** Completed
- **Validation:** `cargo test --manifest-path core/Cargo.toml`【17902e†L1-L24】
- **Notes:** Local and cloud transcripts now derive `is_primary` from `LocalProgress::is_degraded()`, keeping Whisper output primary until a degradation is recorded while regressions cover prefer-cloud sessions and cloud gating under backpressure.【F:core/src/orchestrator/mod.rs†L666-L736】【F:core/src/orchestrator/mod.rs†L800-L878】【F:core/src/orchestrator/mod.rs†L1549-L1680】

### Waveform bridge still runs at ASR cadence
- **Status:** Completed
- **Validation:** `cargo test --manifest-path core/Cargo.toml`【17902e†L1-L24】
- **Notes:** `AudioPipeline` now feeds a dedicated waveform accumulator that emits RMS/VAD telemetry roughly every 32 ms and flushes the tail on shutdown, giving the UI a 30–60 fps waveform stream independent of the 100–200 ms ASR cadence.【F:core/src/audio/mod.rs†L13-L208】【F:core/src/audio/mod.rs†L344-L418】【F:core/src/audio/mod.rs†L420-L468】

### Local send failures drop the fallback notice
- **Status:** Completed
- **Validation:** `cargo test --manifest-path core/Cargo.toml`【e8a4e1†L1-L23】
- **Notes:** Local send failures now enqueue the WARN-level fallback notice used by the latency watchdogs, so session consumers receive the rollback prompt even when Whisper backpressures.【F:core/src/orchestrator/mod.rs†L706-L727】【F:docs/sprint/sprint2.md†L8-L11】

### Cadence timeouts promote cloud silently
- **Status:** Completed
- **Validation:** `cargo test --manifest-path core/Cargo.toml`【e8a4e1†L1-L23】
- **Notes:** The cloud gate now publishes the WARN-level fallback notice before releasing degraded transcripts, guaranteeing every cadence miss surfaces the mandated rollback prompt while keeping Whisper-first ordering tests green.【F:core/src/orchestrator/mod.rs†L812-L834】【F:core/src/orchestrator/mod.rs†L1520-L1583】【F:docs/sprint/sprint2.md†L8-L11】

25. **Fallback promotion lags cadence timeout** — *Status: Completed*
   - Remediation: The cloud worker now tracks when the gating loop times out, marks `LocalProgress` degraded, and notifies waiters before releasing the fallback transcript so the accompanying cloud diff is emitted as primary alongside the degradation notice.【F:core/src/orchestrator/mod.rs†L722-L776】【F:core/src/orchestrator/mod.rs†L121-L210】
   - Validation: `cargo test --manifest-path core/Cargo.toml`【499bb4†L1-L19】

26. **Cloud-preferred sessions suppress Whisper-first primacy** — *Status: Completed*
   - Remediation: Local transcripts now compute primacy from the degradation state rather than the session preference, keeping Whisper output primary until a degradation event records and only then allowing cloud promotion in line with UC2.1’s local-first mandate.【F:core/src/orchestrator/mod.rs†L647-L706】【F:docs/sprint/sprint2.md†L8-L11】【F:docs/architecture.md†L30-L33】
   - Validation: `cargo test --manifest-path core/Cargo.toml`【499bb4†L1-L19】

27. **Cloud promotion still overrides Whisper while healthy** — *Status: Completed*
   - Issue: With `prefer_cloud = true`, `spawn_local_task` cleared primary status after the first local diff while `spawn_cloud_task` always emitted `is_primary = true`, so cloud transcripts overtook Whisper without a recorded degradation, violating the UC2.1 local-first mandate.【F:core/src/orchestrator/mod.rs†L671-L712】【F:core/src/orchestrator/mod.rs†L821-L844】【F:docs/sprint/sprint2.md†L8-L11】【F:docs/architecture.md†L28-L31】
   - Resolution: Primacy is now sourced solely from `LocalProgress::is_degraded()` for both local and cloud paths, keeping Whisper transcripts primary until degradation is detected and promoting cloud results only after a watchdog miss or decoder fault, with regressions covering the prefer-cloud pathway.【F:core/src/orchestrator/mod.rs†L666-L736】【F:core/src/orchestrator/mod.rs†L800-L878】【F:core/src/orchestrator/mod.rs†L1549-L1680】

28. **Waveform bridge still runs at ASR cadence** — *Status: Completed*
   - Issue: `AudioPipeline::emit_chunk` published waveform telemetry once per 100–200 ms ASR chunk (≈5–10 fps), short of the 30–60 fps feedback loop mandated by the architecture's waveform bridge, so the UI could not meet UC2.1's immediate waveform feedback expectation.【F:core/src/audio/mod.rs†L180-L269】【F:docs/architecture.md†L92-L99】
   - Resolution: The audio pipeline now maintains a dedicated waveform accumulator that emits RMS/VAD frames every ~32 ms and flushes the tail on shutdown, decoupling waveform telemetry from ASR chunking while preserving cadence-compliant PCM delivery.【F:core/src/audio/mod.rs†L13-L208】【F:core/src/audio/mod.rs†L344-L418】

29. **Local send failures drop the fallback notice** — *Status: Completed*
   - Remediation: Local send failures now enqueue the WARN-level fallback notice shared with the latency watchdog so downstream consumers receive the mandated rollback prompt even when Whisper backpressures.【F:core/src/orchestrator/mod.rs†L706-L727】【F:docs/sprint/sprint2.md†L8-L11】
   - Validation: `cargo test --manifest-path core/Cargo.toml`【e8a4e1†L1-L23】

30. **Cadence timeouts promote cloud silently** — *Status: Completed*
   - Remediation: The cloud gating loop now publishes the WARN-level fallback notice as soon as it times out waiting for Whisper, ensuring each cadence breach surfaces the UC2.1 rollback messaging even when the watchdog sees a degraded state.【F:core/src/orchestrator/mod.rs†L812-L834】【F:core/src/orchestrator/mod.rs†L1520-L1688】【F:docs/sprint/sprint2.md†L8-L11】
   - Validation: `cargo test --manifest-path core/Cargo.toml`【e8a4e1†L1-L23】

31. **Cloud gate trips during silence** — *Status: Completed*
   - Validation: `cargo test --manifest-path core/Cargo.toml`【e63d5b†L1-L5】
   - Notes: The cloud gating loop now checks both `LocalProgress::has_speech_started()` and `is_speech_active()` before declaring a timeout, preventing WARN fallback notices from firing during natural pauses while keeping UC2.1’s real-anomaly rollback requirement intact.【F:core/src/orchestrator/mod.rs†L803-L855】【F:docs/sprint/sprint2.md†L8-L11】【F:docs/architecture.md†L87-L104】

32. **Waveform telemetry bursts instead of 30–60 fps pacing** — *Status: Completed*
   - Validation: `cargo test --manifest-path core/Cargo.toml`【e63d5b†L1-L5】
   - Notes: `AudioPipeline` now drives waveform telemetry through a dedicated 32 ms ticker that feeds a queued accumulator, pads trailing slices, and emits a silence pre-roll so the UI receives smooth 30–60 fps updates ahead of the first PCM batch.【F:core/src/audio/mod.rs†L23-L204】【F:core/src/audio/mod.rs†L330-L360】【F:docs/architecture.md†L87-L105】

33. **Pending PCM tail flush happens after subscriber removal** — *Status: Completed*
   - Issue: `SessionManager::start_realtime_transcription` only calls `AudioPipeline::flush_pending` once the realtime frame sender has already closed, so `collect_subscribers` drops the orchestrator subscriber and the padded tail never reaches Whisper, violating the 100–200 ms delivery contract.【F:core/src/session/mod.rs†L61-L104】【F:core/src/audio/mod.rs†L265-L327】【F:docs/sprint/sprint2.md†L8-L11】
   - Remediation: The session manager now flushes pending PCM while the subscriber remains registered and drains the resulting frames before releasing the channel, ensuring Whisper receives the padded tail before teardown.【F:core/src/session/mod.rs†L61-L114】【F:core/src/audio/mod.rs†L265-L327】【F:docs/architecture.md†L96-L103】
   - Validation: `cargo test --manifest-path core/Cargo.toml`【59bd29†L1-L4】

34. **Per-session client backpressure blocks the broadcast bus** — *Status: Completed*
   - Issue: The updates relay task awaits `client_tx.send` in the same loop that forwards transcripts to the broadcast channel; a slow subscriber therefore stalls the orchestrator drain and suppresses the mandated 200 ms cadence updates and fallback notices.【F:core/src/session/mod.rs†L90-L104】【F:docs/sprint/sprint2.md†L8-L11】【F:docs/architecture.md†L92-L107】
   - Remediation: Broadcast delivery now remains synchronous while per-session queues use non-blocking `try_send`, dropping only the affected client’s backlog so slow consumers no longer block orchestrator cadence or fallback notices.【F:core/src/session/mod.rs†L99-L120】【F:docs/architecture.md†L92-L107】
   - Validation: `cargo test --manifest-path core/Cargo.toml`【59bd29†L1-L4】

35. **Realtime notices dropped for slow clients** — *Status: Completed*
   - Remediation: Realtime relays now detect WARN/Error notices and await a blocking `send` when the client queue is saturated, guaranteeing UC2.1 fallback prompts are delivered while continuing to drop only best-effort transcript diffs for slow consumers.【F:core/src/session/mod.rs†L107-L148】
   - Validation: `delivers_warn_notice_to_slow_clients` fills a capacity-one queue and asserts the WARN/Error notice is received by both the per-session client and the broadcast bus before cloud fallback resumes.【F:core/src/session/mod.rs†L210-L277】【dacffe†L5-L25】

36. **Local engine falls back to stub when Whisper init fails** — *Status: Completed*
   - Remediation: `EngineOrchestrator::new` now propagates Whisper initialisation failures and only permits the stub path when `WHISPER_ALLOW_FALLBACK` is explicitly set, surfacing misconfiguration instead of silently downgrading UC2.1’s local-first engine.【F:core/src/orchestrator/mod.rs†L39-L48】【F:core/src/orchestrator/mod.rs†L230-L255】
   - Validation: `fails_when_whisper_env_missing_without_fallback` and `allows_fallback_when_explicitly_opted_in` cover the failure and opt-in paths so CI enforces Whisper availability by default.【F:core/src/orchestrator/mod.rs†L1181-L1204】【dacffe†L5-L25】

### Round 23 Summary
- **Status:** No new must-have gaps identified; the latest review confirmed the orchestrator still enforces Whisper-first gating with WARN-backed fallback promotion, the audio pipeline maintains ordered 100–200 ms delivery plus waveform cadence, and the session manager guarantees lossless PCM flushes and reliable WARN/Error notices for slow clients, keeping UC2.1 acceptance criteria satisfied.【F:core/src/orchestrator/mod.rs†L645-L909】【F:core/src/audio/mod.rs†L218-L360】【F:core/src/session/mod.rs†L71-L148】【F:docs/sprint/sprint2.md†L8-L11】
