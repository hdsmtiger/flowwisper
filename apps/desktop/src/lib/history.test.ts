import { invoke } from "@tauri-apps/api/core";
import { beforeEach, describe, expect, it, vi } from "vitest";

import {
  clearHistoryCache,
  loadHistoryEntry,
  markHistoryAccuracy,
  recordHistoryAction,
  searchHistory,
} from "./history";

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

const invokeMock = vi.mocked(invoke);

describe("history client", () => {
  beforeEach(() => {
    invokeMock.mockReset();
    clearHistoryCache();
  });

  it("invokes history search and caches the result", async () => {
    invokeMock.mockResolvedValueOnce({
      entries: [
        {
          sessionId: "s-1",
          startedAtMs: 1,
          completedAtMs: 2,
          durationMs: 1,
          rawTranscript: "raw",
          polishedTranscript: "polished",
          preview: "polished",
          accuracyFlag: "unknown",
          postActions: [],
          metadata: {},
        },
      ],
      nextOffset: null,
      total: 1,
    });

    const first = await searchHistory({ keyword: "hello" });
    expect(first.entries).toHaveLength(1);
    expect(invokeMock).toHaveBeenCalledWith("session_history_search", {
      query: {
        keyword: "hello",
      },
    });

    await searchHistory({ keyword: "hello" });
    expect(invokeMock).toHaveBeenCalledTimes(1);
  });

  it("hydrates cache and reuses entries for direct loads", async () => {
    invokeMock.mockResolvedValueOnce({
      entries: [
        {
          sessionId: "s-cache",
          startedAtMs: 1,
          completedAtMs: 2,
          durationMs: 1,
          rawTranscript: "raw",
          polishedTranscript: "polished",
          preview: "polished",
          accuracyFlag: "unknown",
          postActions: [],
          metadata: {},
        },
      ],
      nextOffset: null,
      total: 1,
    });

    await searchHistory();
    const entry = await loadHistoryEntry("s-cache");
    expect(entry?.sessionId).toBe("s-cache");
    // Second load should hit cache without additional invoke.
    await loadHistoryEntry("s-cache");
    expect(invokeMock).toHaveBeenCalledTimes(1);
  });

  it("forwards accuracy updates", async () => {
    invokeMock.mockResolvedValue(undefined);
    await markHistoryAccuracy({ sessionId: "s-acc", flag: "accurate" });
    expect(invokeMock).toHaveBeenCalledWith("session_history_mark_accuracy", {
      update: {
        sessionId: "s-acc",
        flag: "accurate",
      },
    });
  });

  it("records history actions", async () => {
    invokeMock.mockResolvedValue([{ kind: "copy", timestampMs: 1, detail: {} }]);
    const actions = await recordHistoryAction({
      sessionId: "s-action",
      action: "copy",
    });
    expect(actions).toHaveLength(1);
    expect(invokeMock).toHaveBeenCalledWith("session_history_append_action", {
      request: {
        sessionId: "s-action",
        action: "copy",
        detail: undefined,
      },
    });
  });

  it("invalidates caches after accuracy updates", async () => {
    invokeMock.mockResolvedValueOnce({
      entries: [
        {
          sessionId: "s-acc-cache",
          startedAtMs: 1,
          completedAtMs: 2,
          durationMs: 1,
          rawTranscript: "raw",
          polishedTranscript: "polished",
          preview: "polished",
          accuracyFlag: "unknown",
          postActions: [],
          metadata: {},
        },
      ],
      nextOffset: null,
      total: 1,
    });

    await searchHistory();
    expect(invokeMock).toHaveBeenCalledWith("session_history_search", { query: {} });

    invokeMock.mockResolvedValueOnce(undefined);
    await markHistoryAccuracy({ sessionId: "s-acc-cache", flag: "accurate" });

    invokeMock.mockResolvedValueOnce({
      entries: [],
      nextOffset: null,
      total: 0,
    });

    await searchHistory();
    expect(invokeMock).toHaveBeenLastCalledWith("session_history_search", { query: {} });
  });

  it("refetches search results after recording history actions", async () => {
    invokeMock.mockResolvedValueOnce({
      entries: [
        {
          sessionId: "s-action-cache",
          startedAtMs: 1,
          completedAtMs: 2,
          durationMs: 1,
          rawTranscript: "raw",
          polishedTranscript: "polished",
          preview: "polished",
          accuracyFlag: "unknown",
          postActions: [],
          metadata: {},
        },
      ],
      nextOffset: null,
      total: 1,
    });

    await searchHistory();

    invokeMock.mockResolvedValueOnce([
      { kind: "copy", timestampMs: 1, detail: {} },
    ]);
    await recordHistoryAction({
      sessionId: "s-action-cache",
      action: "copy",
    });

    invokeMock.mockResolvedValueOnce({
      entries: [
        {
          sessionId: "s-action-cache",
          startedAtMs: 1,
          completedAtMs: 2,
          durationMs: 1,
          rawTranscript: "raw",
          polishedTranscript: "polished",
          preview: "polished",
          accuracyFlag: "unknown",
          postActions: [
            { kind: "copy", timestampMs: 1, detail: {} },
          ],
          metadata: {},
        },
      ],
      nextOffset: null,
      total: 1,
    });

    await searchHistory();
    expect(invokeMock).toHaveBeenLastCalledWith("session_history_search", { query: {} });
  });
});
