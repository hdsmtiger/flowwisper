# UC2.1 Review (Round 13)

## Context
Evaluate the ordered PCM fan-out patch against Sprint 2 UC2.1’s local-first latency goal and the architecture’s audio pipeline budget (100–200 ms frames, <400 ms first local transcript, bounded buffering).【F:docs/sprint/sprint2.md†L8-L11】【F:docs/architecture.md†L187-L202】

## Findings

1. **Cloud can still publish before Whisper under backpressure** — *Status: Completed*
   - The local worker marks `LocalProgress` as successful *before* awaiting the `tx.send` that enqueues the Whisper diff.【F:core/src/orchestrator/mod.rs†L642-L668】 Because the cloud task polls `local_progress.last_frame()` inside its gating loop, a congested updates channel allows the cloud branch to proceed once the flag flips, letting a faster cloud transcript reach clients ahead of the still-pending local send. This breaks UC2.1’s requirement that the first visible characters originate from the local Whisper path within 400 ms.【F:docs/sprint/sprint2.md†L8-L11】
   - **Resolution:** `spawn_local_task` now claims the “first update” flag without advancing `LocalProgress` until `tx.send` succeeds, only setting the Whisper-first notifier after the update is enqueued and rolling back the claim on failure, so the cloud branch keeps waiting while the local send is back-pressured.【F:core/src/orchestrator/mod.rs†L632-L708】
   - **Validation:** Added `cloud_does_not_preempt_when_updates_channel_full` to lock the Whisper-first guarantee when the updates channel is saturated.【F:core/src/orchestrator/mod.rs†L1397-L1474】

2. **Per-subscriber PCM queues are unbounded** — *Status: Completed*
   - `PcmSubscriber::enqueue` pushes every frame into a `VecDeque` without any capacity guard, so a stalled subscriber accumulates the entire session in memory while background senders await channel capacity.【F:core/src/audio/mod.rs†L33-L89】 This violates the architecture budget that limits in-memory audio buffering to a few minutes (<80 MB) and expects backpressure or shedding once consumers lag.【F:docs/architecture.md†L187-L202】
   - **Resolution:** Each PCM subscriber now tracks a bounded queue sized to a small multiple of its channel capacity, drops the oldest frame when the limit is exceeded, and logs the shed frame so stalled consumers can be alerted while preserving the memory cap.【F:core/src/audio/mod.rs†L26-L150】

## Status
- Gap 17 (Cloud preempts Whisper under send backpressure) — *Completed*.
- Gap 18 (PCM subscriber queues lack bounds) — *Completed*.
