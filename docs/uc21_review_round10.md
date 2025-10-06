# UC2.1 Review (Round 10)

## Context
Evaluate the post–round-nine realtime transcription code against Sprint 2 UC2.1, the PRD, and the architecture mandate that Whisper must deliver the first characters within 400 ms even when cloud decoding is enabled.【F:docs/sprint/sprint2.md†L8-L11】【F:docs/architecture.md†L28-L33】

## Findings

1. **Cloud-preferred sessions still violate the Whisper-first SLA**
   - The cloud worker skips the gating branch whenever `prefer_cloud = true`, so it can publish the very first transcript before the Whisper path responds. In a real deployment where Azure Speech routinely returns earlier than Whisper, the watchdog will be satisfied by the cloud diff even though the UC2.1 acceptance criterion (“Whisper 本地引擎在 400 ms 内输出首批原始转写字符”) was never met.【F:core/src/orchestrator/mod.rs†L732-L775】
   - Because the first-update flag is flipped by the cloud branch, subsequent Whisper text is no longer marked as the first diff, leaving the UI without the required “本地首字” experience and undermining the privacy-first promise in the architecture doc.【F:core/src/orchestrator/mod.rs†L760-L775】【F:docs/architecture.md†L28-L33】
   - **Recommendation:** Always gate the initial cloud emission on the local first-update flag (or wait for a Whisper completion notice plus the deadline violation) regardless of `prefer_cloud`, and add a regression test that proves Whisper remains the first visible transcript even when the cloud engine answers earlier.

## Status
- Gap recorded in `uc21_review_gaps.md` as item 14 and left open pending remediation.
