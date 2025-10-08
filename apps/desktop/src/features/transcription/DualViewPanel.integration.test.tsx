import { act, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import { beforeEach, describe, expect, it, vi } from "vitest";

import type { DualViewPanelProps } from "./DualViewPanel";

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
        notice: { level: "info" | "warn" | "error"; message: string };
      }
    | {
        type: "selection";
        selections: { sentenceId: number; activeVariant: "raw" | "polished" }[];
      };
};

type PublishStrategy = "directInsert" | "clipboardFallback" | "notifyOnly";

type FallbackStrategy = "clipboardCopy" | "notifyOnly" | null;

type PublishStatus = "completed" | "deferred" | "failed";

type PublishingUpdate = {
  sessionId: string;
  attempt: number;
  strategy: PublishStrategy;
  fallback: FallbackStrategy;
  retrying: boolean;
  detail: string | null;
  timestampMs: number;
};

type InsertionResult = {
  sessionId: string;
  status: PublishStatus;
  strategy: PublishStrategy;
  attempts: number;
  fallback: Exclude<FallbackStrategy, null> | null;
  failure: { code: string | null; message: string } | null;
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

const { useDualViewTranscript } = await import("./hooks/useDualViewTranscript");
const { DualViewPanel } = await import("./DualViewPanel");
const { invoke } = await import("@tauri-apps/api/core");
const { listen } = await import("@tauri-apps/api/event");

const TRANSCRIPT_EVENT_CHANNEL = "session://transcript";
const LIFECYCLE_EVENT_CHANNEL = "session://lifecycle";
const PUBLISH_RESULT_CHANNEL = "session://publish-result";
const PUBLISH_NOTICE_CHANNEL = "session://publish-notice";

const emit = <T,>(channel: string, payload: T) => {
  (listeners[channel] || []).forEach((handler) => handler({ payload }));
};

const Harness = (props: Partial<DualViewPanelProps>) => {
  const transcript = useDualViewTranscript();
  return <DualViewPanel transcript={transcript} {...props} />;
};

const createTranscriptEvent = (
  overrides: Partial<TranscriptStreamEvent>,
): TranscriptStreamEvent => ({
  timestampMs: 0,
  frameIndex: 0,
  latencyMs: 120,
  isFirst: false,
  payload: {
    type: "transcript",
    sentence: {
      sentenceId: 1,
      text: "",
      source: "local",
      isPrimary: true,
      withinSla: true,
    },
  },
  ...overrides,
});

const ensureBridge = () => {
  const globalWithBridge = globalThis as typeof globalThis & {
    __TAURI__?: Record<string, unknown>;
  };
  globalWithBridge.__TAURI__ = {};
};

describe("DualViewPanel integration", () => {
  beforeEach(() => {
    ensureBridge();
    (invoke as unknown as ReturnType<typeof vi.fn>).mockReset();
    (listen as unknown as ReturnType<typeof vi.fn>).mockClear();
    Object.keys(listeners).forEach((key) => delete listeners[key]);
  });

  it("renders live transcript updates across both variants", async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockImplementation(
      async (command: string) => {
        if (command === "session_transcript_log") {
          return [];
        }
        if (
          command === "session_publish_history" ||
          command === "session_publish_results" ||
          command === "session_notice_center_history"
        ) {
          return [];
        }
        return null;
      },
    );

    render(<Harness emptyState="Waiting for transcript" />);

    await waitFor(() => {
      expect(listen).toHaveBeenCalledWith(
        TRANSCRIPT_EVENT_CHANNEL,
        expect.any(Function),
      );
    });

    await act(async () => {
      emit(
        TRANSCRIPT_EVENT_CHANNEL,
        createTranscriptEvent({
          timestampMs: 100,
          frameIndex: 0,
          isFirst: true,
          payload: {
            type: "transcript",
            sentence: {
              sentenceId: 7,
              text: "Raw stream sentence",
              source: "local",
              isPrimary: true,
              withinSla: true,
            },
          },
        }),
      );
    });

    const rawList = await screen.findByRole("list", {
      name: /original transcript sentences/i,
    });
    expect(
      within(rawList).getByText("Raw stream sentence"),
    ).toBeInTheDocument();

    await act(async () => {
      emit(
        TRANSCRIPT_EVENT_CHANNEL,
        createTranscriptEvent({
          timestampMs: 220,
          frameIndex: 1,
          payload: {
            type: "transcript",
            sentence: {
              sentenceId: 7,
              text: "Polished stream sentence",
              source: "polished",
              isPrimary: true,
              withinSla: true,
            },
          },
        }),
      );
    });

    const polishedList = await screen.findByRole("list", {
      name: /polished transcript sentences/i,
    });
    expect(
      within(polishedList).getByText("Polished stream sentence"),
    ).toBeInTheDocument();
    expect(
      within(polishedList).getByRole("button", {
        name: /use original sentence 7/i,
      }),
    ).toBeInTheDocument();
  });

  it("supports multi-select revert flows", async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockImplementation(
      async (command: string, args?: unknown) => {
        if (command === "session_transcript_log") {
          return [];
        }
        if (command === "session_transcript_apply_selection") {
          return args;
        }
        if (
          command === "session_publish_history" ||
          command === "session_publish_results" ||
          command === "session_notice_center_history"
        ) {
          return [];
        }
        return null;
      },
    );

    render(<Harness />);

    await waitFor(() => {
      expect(listen).toHaveBeenCalledWith(
        TRANSCRIPT_EVENT_CHANNEL,
        expect.any(Function),
      );
    });

    const seedSentence = async (id: number) => {
      await act(async () => {
        emit(
          TRANSCRIPT_EVENT_CHANNEL,
          createTranscriptEvent({
            timestampMs: id * 100,
            frameIndex: id * 2,
            payload: {
              type: "transcript",
              sentence: {
                sentenceId: id,
                text: `Raw sentence ${id}`,
                source: "local",
                isPrimary: true,
                withinSla: true,
              },
            },
          }),
        );
      });
      await act(async () => {
        emit(
          TRANSCRIPT_EVENT_CHANNEL,
          createTranscriptEvent({
            timestampMs: id * 100 + 50,
            frameIndex: id * 2 + 1,
            payload: {
              type: "transcript",
              sentence: {
                sentenceId: id,
                text: `Polished sentence ${id}`,
                source: "polished",
                isPrimary: true,
                withinSla: true,
              },
            },
          }),
        );
      });
    };

    await seedSentence(1);
    await seedSentence(2);

    const selectButtons = await screen.findAllByRole("button", {
      name: /select sentence/i,
    });

    await act(async () => {
      fireEvent.click(selectButtons[0]);
      fireEvent.click(selectButtons[1]);
    });

    expect(screen.getByText("2/5 selected")).toBeInTheDocument();

    const revertButton = screen.getByRole("button", {
      name: /revert selected sentences to original/i,
    });

    await act(async () => {
      fireEvent.click(revertButton);
    });

    expect(invoke).toHaveBeenCalledWith(
      "session_transcript_apply_selection",
      {
        selections: [
          { sentenceId: 1, activeVariant: "raw" },
          { sentenceId: 2, activeVariant: "raw" },
        ],
      },
    );

    await act(async () => {
      emit(
        TRANSCRIPT_EVENT_CHANNEL,
        createTranscriptEvent({
          timestampMs: 999,
          frameIndex: 9,
          payload: {
            type: "selection",
            selections: [
              { sentenceId: 1, activeVariant: "raw" },
              { sentenceId: 2, activeVariant: "raw" },
            ],
          },
        }),
      );
    });

    await waitFor(() => {
      expect(screen.queryByText(/selected/)).not.toBeInTheDocument();
    });
  });

  it("honors keyboard shortcuts for sentence focus navigation", async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockImplementation(
      async (command: string) => {
        if (command === "session_transcript_log") {
          return [];
        }
        if (
          command === "session_publish_history" ||
          command === "session_publish_results" ||
          command === "session_notice_center_history"
        ) {
          return [];
        }
        return null;
      },
    );

    render(<Harness />);

    await waitFor(() => {
      expect(listen).toHaveBeenCalledWith(
        TRANSCRIPT_EVENT_CHANNEL,
        expect.any(Function),
      );
    });

    const populate = async (id: number) => {
      await act(async () => {
        emit(
          TRANSCRIPT_EVENT_CHANNEL,
          createTranscriptEvent({
            timestampMs: id * 200,
            frameIndex: id * 2,
            payload: {
              type: "transcript",
              sentence: {
                sentenceId: id,
                text: `Sentence ${id}`,
                source: "local",
                isPrimary: true,
                withinSla: true,
              },
            },
          }),
        );
      });
    };

    await populate(1);
    await populate(2);
    await populate(3);

    const rawList = await screen.findByRole("list", {
      name: /original transcript sentences/i,
    });
    const rawItems = within(rawList).getAllByRole("listitem");

    await waitFor(() => {
      expect(rawItems[0]).toHaveFocus();
    });

    await act(async () => {
      fireEvent.keyDown(rawItems[0], { key: "ArrowDown", code: "ArrowDown" });
    });

    await waitFor(() => {
      expect(rawItems[1]).toHaveFocus();
    });

    await act(async () => {
      fireEvent.keyDown(rawItems[1], { key: "ArrowUp", code: "ArrowUp" });
    });

    await waitFor(() => {
      expect(rawItems[0]).toHaveFocus();
    });
  });

  it("surfaces publishing retries, failures, and notices", async () => {
    (invoke as unknown as ReturnType<typeof vi.fn>).mockImplementation(
      async (command: string) => {
        switch (command) {
          case "session_transcript_log":
            return [];
          case "session_publish_history": {
            const history: PublishingUpdate[] = [
              {
                sessionId: "session-x",
                attempt: 1,
                strategy: "directInsert",
                fallback: null,
                retrying: false,
                detail: "Initial insert",
                timestampMs: 1,
              },
            ];
            return history;
          }
          case "session_publish_results":
            return [] as InsertionResult[];
          case "session_notice_center_history":
            return [] as PublishNotice[];
          default:
            return null;
        }
      },
    );

    render(<Harness />);

    await waitFor(() => {
      expect(listen).toHaveBeenCalledWith(
        LIFECYCLE_EVENT_CHANNEL,
        expect.any(Function),
      );
      expect(listen).toHaveBeenCalledWith(
        PUBLISH_RESULT_CHANNEL,
        expect.any(Function),
      );
      expect(listen).toHaveBeenCalledWith(
        PUBLISH_NOTICE_CHANNEL,
        expect.any(Function),
      );
    });

    await waitFor(() => {
      expect(
        screen.getByText(/Publishing polished transcript…/i),
      ).toBeInTheDocument();
    });

    const retryUpdate: PublishingUpdate = {
      sessionId: "session-x",
      attempt: 2,
      strategy: "clipboardFallback",
      fallback: "clipboardCopy",
      retrying: true,
      detail: "Retry clipboard fallback",
      timestampMs: 2,
    };

    await act(async () => {
      emit(LIFECYCLE_EVENT_CHANNEL, retryUpdate);
    });

    await waitFor(() => {
      expect(screen.getByText(/Retrying…/i)).toBeInTheDocument();
    });

    const failureResult: InsertionResult = {
      sessionId: "session-x",
      status: "failed",
      strategy: "clipboardFallback",
      attempts: 2,
      fallback: "clipboardCopy",
      failure: { code: "focus-lost", message: "Target window lost focus" },
      undoToken: "undo-x",
      timestampMs: 3,
    };

    await act(async () => {
      emit(PUBLISH_RESULT_CHANNEL, failureResult);
    });

    await waitFor(() => {
      expect(
        screen.getByText(/Automatic insertion failed after 2 attempt/i),
      ).toBeInTheDocument();
      expect(
        screen.getByText(/Copied the transcript to your clipboard as a fallback/i),
      ).toBeInTheDocument();
    });

    const undoNotice: PublishNotice = {
      sessionId: "session-x",
      action: "undoPrompt",
      level: "warn",
      message: "Press Ctrl+Z to undo",
      undoToken: "undo-x",
      timestampMs: 4,
    };

    await act(async () => {
      emit(PUBLISH_NOTICE_CHANNEL, undoNotice);
    });

    await waitFor(() => {
      expect(
        screen.getByText(/Undo is available via Ctrl\/Cmd\+Z or from the clipboard backup/i),
      ).toBeInTheDocument();
      expect(screen.getAllByText(/Press Ctrl\+Z to undo/i).length).toBeGreaterThan(0);
    });

    const historyToggle = screen.getByRole("button", { name: /show history/i });
    fireEvent.click(historyToggle);

    const attemptsList = await screen.findByText(/Publishing attempts/i);
    expect(attemptsList).toBeInTheDocument();
    expect(screen.getByText(/Attempt 2/i)).toBeInTheDocument();
    expect(screen.getByText(/Detail: Retry clipboard fallback/i)).toBeInTheDocument();
    expect(screen.getByText(/Recent notices/i)).toBeInTheDocument();
    expect(screen.getAllByText(/Press Ctrl\+Z to undo/i).length).toBeGreaterThan(0);
  });
});
