import { act, renderHook, waitFor } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

type TranscriptStreamSource = "local" | "cloud" | "polished";

type TranscriptStreamEvent = {
  timestampMs: number;
  frameIndex: number;
  latencyMs: number;
  isFirst: boolean;
  payload:
    | {
        type: "transcript";
        sentence: {
          sentenceId: number;
          text: string;
          source: TranscriptStreamSource;
          isPrimary: boolean;
          withinSla: boolean;
        };
      }
    | {
        type: "notice";
        notice: {
          level: "info" | "warn" | "error";
          message: string;
        };
      }
    | {
        type: "selection";
        selections: { sentenceId: number; activeVariant: "raw" | "polished" }[];
      };
};

type PublishStrategy = "directInsert" | "clipboardFallback" | "notifyOnly";

type FallbackStrategy = "clipboardCopy" | "notifyOnly";

type PublishStatus = "completed" | "deferred" | "failed";

type PublishingUpdate = {
  sessionId: string;
  attempt: number;
  strategy: PublishStrategy;
  fallback: FallbackStrategy | null;
  retrying: boolean;
  detail: string | null;
  timestampMs: number;
};

type InsertionFailure = {
  code: string | null;
  message: string;
};

type InsertionResult = {
  sessionId: string;
  status: PublishStatus;
  strategy: PublishStrategy;
  attempts: number;
  fallback: FallbackStrategy | null;
  failure: InsertionFailure | null;
  undoToken: string | null;
  timestampMs: number;
};

type PublishNotice = {
  sessionId: string;
  action: "insert" | "copy" | "saveDraft" | "undoPrompt";
  level: "info" | "warn" | "error";
  message: string;
  undoToken: string | null;
  timestampMs: number;
};

type Listener = (event: { payload: unknown }) => void;

const listeners: Record<string, Listener[]> = {};

vi.mock("@tauri-apps/api/core", () => ({
  invoke: vi.fn(),
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn((channel: string, handler: Listener) => {
    listeners[channel] = listeners[channel] || [];
    listeners[channel].push(handler);
    return Promise.resolve(() => {
      listeners[channel] = (listeners[channel] || []).filter(
        (current) => current !== handler,
      );
    });
  }),
}));

const emit = <T,>(channel: string, payload: T) => {
  (listeners[channel] || []).forEach((handler) => handler({ payload }));
};

const mockInvokeResponses = (responses?: {
  transcript?: TranscriptStreamEvent[];
  updates?: PublishingUpdate[];
  results?: InsertionResult[];
  notices?: PublishNotice[];
}) => {
  const payload = {
    transcript: responses?.transcript ?? [],
    updates: responses?.updates ?? [],
    results: responses?.results ?? [],
    notices: responses?.notices ?? [],
  };
  (invoke as unknown as ReturnType<typeof vi.fn>).mockImplementation(
    (command: string) => {
      switch (command) {
        case "session_transcript_log":
          return Promise.resolve(payload.transcript);
        case "session_publish_history":
          return Promise.resolve(payload.updates);
        case "session_publish_results":
          return Promise.resolve(payload.results);
        case "session_notice_center_history":
          return Promise.resolve(payload.notices);
        default:
          return Promise.resolve([]);
      }
    },
  );
};

const { useDualViewTranscript, MAX_MULTI_SELECT } = await import(
  "./useDualViewTranscript"
);
const { invoke } = await import("@tauri-apps/api/core");
const { listen } = await import("@tauri-apps/api/event");

describe("useDualViewTranscript", () => {
  const TRANSCRIPT_EVENT_CHANNEL = "session://transcript";
  const LIFECYCLE_EVENT_CHANNEL = "session://lifecycle";
  const PUBLISH_RESULT_CHANNEL = "session://publish-result";
  const PUBLISH_NOTICE_CHANNEL = "session://publish-notice";

  beforeEach(() => {
    (globalThis as Record<string, unknown>).__TAURI__ = {};
    (invoke as unknown as ReturnType<typeof vi.fn>).mockReset();
    (listen as unknown as ReturnType<typeof vi.fn>).mockClear();
    Object.keys(listeners).forEach((key) => delete listeners[key]);
    mockInvokeResponses();
  });

  it("hydrates existing transcript history and exposes variants", async () => {
    const history: TranscriptStreamEvent[] = [
      {
        timestampMs: 10,
        frameIndex: 0,
        latencyMs: 90,
        isFirst: true,
        payload: {
          type: "transcript",
          sentence: {
            sentenceId: 1,
            text: "Hello world",
            source: "local",
            isPrimary: true,
            withinSla: true,
          },
        },
      },
      {
        timestampMs: 160,
        frameIndex: 1,
        latencyMs: 2150,
        isFirst: false,
        payload: {
          type: "transcript",
          sentence: {
            sentenceId: 1,
            text: "Hello world polished",
            source: "polished",
            isPrimary: true,
            withinSla: true,
          },
        },
      },
    ];

    mockInvokeResponses({ transcript: history });

    const { result } = renderHook(() => useDualViewTranscript());

    await waitFor(() => {
      expect(result.current.isHydrated).toBe(true);
      expect(result.current.sentences).toHaveLength(1);
    });

    const sentence = result.current.sentences[0];
    expect(sentence.activeVariant).toBe("polished");
    expect(sentence.raw?.text).toBe("Hello world");
    expect(sentence.polished?.text).toBe("Hello world polished");
    expect(sentence.raw?.source).toBe("local");
    expect(sentence.polished?.source).toBe("polished");
    expect(sentence.ariaLabel).toContain("Polished version active");
    expect(result.current.focusedSentenceId).toBe(1);
    expect(result.current.announcements.length).toBeGreaterThan(0);
  });

  it("handles live transcript events and selection acknowledgements", async () => {
    const { result } = renderHook(() => useDualViewTranscript());

    await waitFor(() => {
      expect(result.current.isHydrated).toBe(true);
    });

    await waitFor(() => {
      expect(listeners[TRANSCRIPT_EVENT_CHANNEL]).toBeDefined();
    });

    act(() => {
      emit(TRANSCRIPT_EVENT_CHANNEL, {
        timestampMs: 25,
        frameIndex: 0,
        latencyMs: 110,
        isFirst: true,
        payload: {
          type: "transcript",
          sentence: {
            sentenceId: 42,
            text: "Raw sentence",
            source: "cloud",
            isPrimary: true,
            withinSla: false,
          },
        },
      });
    });

    await waitFor(() => {
      expect(result.current.sentences).toHaveLength(1);
    });

    expect(result.current.sentences[0].activeVariant).toBe("raw");
    expect(result.current.sentences[0].raw?.withinSla).toBe(false);
    expect(result.current.focusedSentenceId).toBe(42);

    const firstAnnouncement =
      result.current.announcements[result.current.announcements.length - 1];
    expect(firstAnnouncement?.message).toBe(
      "Sentence 42 raw update delayed.",
    );

    act(() => {
      emit(TRANSCRIPT_EVENT_CHANNEL, {
        timestampMs: 900,
        frameIndex: 5,
        latencyMs: 1900,
        isFirst: false,
        payload: {
          type: "selection",
          selections: [
            {
              sentenceId: 42,
              activeVariant: "polished",
            },
          ],
        },
      });
    });

    await waitFor(() => {
      expect(result.current.sentences[0].activeVariant).toBe("polished");
      expect(result.current.selectedSentenceIds).toHaveLength(0);
    });

    const selectionAnnouncement =
      result.current.announcements[result.current.announcements.length - 1];
    expect(selectionAnnouncement?.message).toBe(
      "Sentence 42 active variant set to polished.",
    );

    act(() => {
      result.current.acknowledgeAnnouncement(selectionAnnouncement!.id);
    });

    expect(
      result.current.announcements.some(
        (announcement) => announcement.id === selectionAnnouncement!.id,
      ),
    ).toBe(false);

    act(() => {
      emit(TRANSCRIPT_EVENT_CHANNEL, {
        timestampMs: 1200,
        frameIndex: 6,
        latencyMs: 180,
        isFirst: false,
        payload: {
          type: "transcript",
          sentence: {
            sentenceId: 43,
            text: "Second sentence", // raw variant
            source: "local",
            isPrimary: true,
            withinSla: true,
          },
        },
      });
    });

    await waitFor(() => {
      expect(result.current.sentences).toHaveLength(2);
    });

    act(() => {
      result.current.focusNextSentence();
    });
    expect(result.current.focusedSentenceId).toBe(43);

    act(() => {
      result.current.focusPreviousSentence();
    });
    expect(result.current.focusedSentenceId).toBe(42);
  });

  it("hydrates and streams publishing lifecycle data", async () => {
    const updates: PublishingUpdate[] = [
      {
        sessionId: "session-1",
        attempt: 1,
        strategy: "directInsert",
        fallback: null,
        retrying: false,
        detail: "Initial attempt",
        timestampMs: 10,
      },
    ];
    const results: InsertionResult[] = [
      {
        sessionId: "session-1",
        status: "completed",
        strategy: "directInsert",
        attempts: 1,
        fallback: null,
        failure: null,
        undoToken: "undo-1",
        timestampMs: 25,
      },
    ];
    const notices: PublishNotice[] = [
      {
        sessionId: "session-1",
        action: "insert",
        level: "info",
        message: "Inserted successfully",
        undoToken: "undo-1",
        timestampMs: 30,
      },
    ];

    mockInvokeResponses({ updates, results, notices });

    const { result } = renderHook(() => useDualViewTranscript());

    await waitFor(() => {
      expect(result.current.publishUpdates).toHaveLength(1);
    });

    expect(result.current.publishResults).toEqual(results);
    expect(result.current.publishNotices).toEqual(notices);

    const nextUpdate: PublishingUpdate = {
      sessionId: "session-1",
      attempt: 2,
      strategy: "clipboardFallback",
      fallback: "clipboardCopy",
      retrying: true,
      detail: "Retry clipboard fallback",
      timestampMs: 45,
    };

    act(() => {
      emit(LIFECYCLE_EVENT_CHANNEL, nextUpdate);
    });

    await waitFor(() => {
      expect(result.current.publishUpdates).toHaveLength(2);
    });

    const nextResult: InsertionResult = {
      sessionId: "session-1",
      status: "deferred",
      strategy: "clipboardFallback",
      attempts: 2,
      fallback: "clipboardCopy",
      failure: {
        code: "timeout",
        message: "Clipboard fallback engaged",
      },
      undoToken: null,
      timestampMs: 60,
    };

    act(() => {
      emit(PUBLISH_RESULT_CHANNEL, nextResult);
    });

    await waitFor(() => {
      expect(result.current.publishResults).toHaveLength(2);
    });
    expect(result.current.publishResults[1]).toEqual(nextResult);

    const nextNotice: PublishNotice = {
      sessionId: "session-1",
      action: "undoPrompt",
      level: "warn",
      message: "Undo is available",
      undoToken: null,
      timestampMs: 75,
    };

    act(() => {
      emit(PUBLISH_NOTICE_CHANNEL, nextNotice);
    });

    await waitFor(() => {
      expect(result.current.publishNotices).toHaveLength(2);
    });
    expect(
      result.current.publishNotices[result.current.publishNotices.length - 1],
    ).toEqual(nextNotice);
  });

  it("tracks local selection state and pending requests", async () => {
    const { result } = renderHook(() => useDualViewTranscript());
    await waitFor(() => {
      expect(result.current.isHydrated).toBe(true);
    });

    act(() => {
      result.current.toggleSelection(5);
    });

    expect(result.current.selectedSentenceIds).toEqual([5]);

    act(() => {
      result.current.markPendingSelection([5], "raw");
    });

    expect(result.current.pendingSelections[5]).toBe("raw");
    const pendingAnnouncement =
      result.current.announcements[result.current.announcements.length - 1];
    expect(pendingAnnouncement?.message).toBe(
      "Pending switch for sentence 5 to raw.",
    );

    act(() => {
      result.current.clearSelections();
    });

    expect(result.current.selectedSentenceIds).toHaveLength(0);
    expect(result.current.pendingSelections).toEqual({});
  });

  it("limits selections to five sentences and surfaces announcements", async () => {
    const { result } = renderHook(() => useDualViewTranscript());

    await waitFor(() => {
      expect(result.current.isHydrated).toBe(true);
    });

    act(() => {
      for (let id = 1; id <= MAX_MULTI_SELECT; id += 1) {
        result.current.toggleSelection(id);
      }
    });

    expect(result.current.selectedSentenceIds).toEqual(
      Array.from({ length: MAX_MULTI_SELECT }, (_, index) => index + 1),
    );

    const announcementCount = result.current.announcements.length;

    act(() => {
      result.current.toggleSelection(MAX_MULTI_SELECT + 1);
    });

    expect(result.current.selectedSentenceIds).toHaveLength(MAX_MULTI_SELECT);
    const latest = result.current.announcements[result.current.announcements.length - 1];
    expect(latest?.message).toMatch(/select up to/i);
    expect(result.current.announcements.length).toBeGreaterThanOrEqual(
      announcementCount,
    );
  });

  it("applies selection commands through Tauri and reports failures", async () => {
    const { result } = renderHook(() => useDualViewTranscript());

    await waitFor(() => {
      expect(result.current.isHydrated).toBe(true);
    });

    act(() => {
      result.current.markPendingSelection([9], "raw");
    });

    await act(async () => {
      const ok = await result.current.applySelection([9], "raw");
      expect(ok).toBe(true);
    });

    expect(invoke).toHaveBeenCalledWith(
      "session_transcript_apply_selection",
      {
        selections: [{ sentenceId: 9, activeVariant: "raw" }],
      },
    );

    (invoke as unknown as ReturnType<typeof vi.fn>).mockRejectedValueOnce(
      new Error("network down"),
    );

    act(() => {
      result.current.markPendingSelection([11], "polished");
    });

    await act(async () => {
      const ok = await result.current.applySelection([11], "polished");
      expect(ok).toBe(false);
    });

    expect(result.current.pendingSelections[11]).toBeUndefined();
    const failureAnnouncement =
      result.current.announcements[result.current.announcements.length - 1];
    expect(failureAnnouncement?.message).toMatch(/failed to update/i);
  });

  it("keeps raw overrides active when later polished updates arrive", async () => {
    const history: TranscriptStreamEvent[] = [
      {
        timestampMs: 10,
        frameIndex: 0,
        latencyMs: 110,
        isFirst: true,
        payload: {
          type: "transcript",
          sentence: {
            sentenceId: 21,
            text: "raw sentence",
            source: "local",
            isPrimary: true,
            withinSla: true,
          },
        },
      },
      {
        timestampMs: 180,
        frameIndex: 1,
        latencyMs: 420,
        isFirst: false,
        payload: {
          type: "transcript",
          sentence: {
            sentenceId: 21,
            text: "polished sentence",
            source: "polished",
            isPrimary: true,
            withinSla: true,
          },
        },
      },
    ];

    mockInvokeResponses({ transcript: history });

    const { result } = renderHook(() => useDualViewTranscript());

    await waitFor(() => {
      expect(result.current.sentences).toHaveLength(1);
      expect(result.current.sentences[0].activeVariant).toBe("polished");
    });

    act(() => {
      emit(TRANSCRIPT_EVENT_CHANNEL, {
        timestampMs: 220,
        frameIndex: 2,
        latencyMs: 900,
        isFirst: false,
        payload: {
          type: "selection",
          selections: [
            {
              sentenceId: 21,
              activeVariant: "raw",
            },
          ],
        },
      });
    });

    await waitFor(() => {
      expect(result.current.sentences[0].activeVariant).toBe("raw");
    });

    act(() => {
      emit(TRANSCRIPT_EVENT_CHANNEL, {
        timestampMs: 400,
        frameIndex: 3,
        latencyMs: 350,
        isFirst: false,
        payload: {
          type: "transcript",
          sentence: {
            sentenceId: 21,
            text: "polished sentence refreshed",
            source: "polished",
            isPrimary: true,
            withinSla: true,
          },
        },
      });
    });

    await waitFor(() => {
      expect(result.current.sentences[0].activeVariant).toBe("raw");
      expect(result.current.sentences[0].polished?.text).toBe(
        "polished sentence refreshed",
      );
    });
  });
});

