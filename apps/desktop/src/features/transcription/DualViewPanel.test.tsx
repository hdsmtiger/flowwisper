import { act, fireEvent, render, screen, within } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import type {
  DualViewNotice,
  DualViewSentence,
  DualViewTranscriptState,
  SentenceVariantState,
} from "./hooks/useDualViewTranscript";
import { DualViewPanel } from "./DualViewPanel";

const buildVariant = (overrides: Partial<SentenceVariantState>) => ({
  text: "",
  source: "local" as const,
  latencyMs: 0,
  withinSla: true,
  lastUpdated: Date.now(),
  ...overrides,
});

const createTranscriptState = (
  overrides: Partial<DualViewTranscriptState> = {},
): DualViewTranscriptState => ({
  sentences: [],
  notices: [],
  publishUpdates: [],
  publishResults: [],
  publishNotices: [],
  selectedSentenceIds: [],
  pendingSelections: {},
  isHydrated: true,
  error: null,
  focusedSentenceId: null,
  announcements: [],
  toggleSelection: vi.fn(),
  selectSentences: vi.fn(),
  clearSelections: vi.fn(),
  markPendingSelection: vi.fn(),
  applySelection: vi.fn().mockResolvedValue(true),
  focusSentence: vi.fn(),
  focusNextSentence: vi.fn(),
  focusPreviousSentence: vi.fn(),
  acknowledgeAnnouncement: vi.fn(),
  ...overrides,
});

describe("DualViewPanel", () => {
  it("renders empty state for each column when no sentences are present", () => {
    const state = createTranscriptState();
    render(<DualViewPanel transcript={state} emptyState="No transcript yet" />);

    const emptyMessages = screen.getAllByText("No transcript yet");
    expect(emptyMessages).toHaveLength(2);
  });

  it("renders raw and polished cards with text content", () => {
    const sentence: DualViewSentence = {
      id: 1,
      firstFrameIndex: 0,
      lastUpdated: Date.now(),
      activeVariant: "raw",
      raw: buildVariant({ text: "raw sentence" }),
      polished: buildVariant({ text: "polished sentence", source: "polished" }),
      pendingVariant: null,
      ariaLabel: "Sentence 1",
    };

    const focusSentence = vi.fn();
    const state = createTranscriptState({
      sentences: [sentence],
      focusedSentenceId: 1,
      focusSentence,
    });

    render(<DualViewPanel transcript={state} />);

    const rawList = screen.getByRole("list", { name: /original transcript/i });
    const polishedList = screen.getByRole("list", { name: /polished transcript/i });

    expect(within(rawList).getByText("raw sentence")).toBeInTheDocument();
    expect(within(polishedList).getByText("polished sentence")).toBeInTheDocument();
    expect(focusSentence).toHaveBeenCalledWith(1);
    expect(
      screen.getByText("Conversational tone · Light grammar fixes"),
    ).toBeInTheDocument();
  });

  it("shows polishing delay messaging when SLA is missed", () => {
    const sentence: DualViewSentence = {
      id: 6,
      firstFrameIndex: 0,
      lastUpdated: Date.now(),
      activeVariant: "polished",
      raw: buildVariant({ text: "raw" }),
      polished: buildVariant({
        text: "polished",
        source: "polished",
        withinSla: false,
      }),
      pendingVariant: null,
      ariaLabel: "Sentence 6",
    };

    const state = createTranscriptState({ sentences: [sentence] });
    render(<DualViewPanel transcript={state} />);

    expect(
      screen.getByText(/polishing is taking longer than expected/i),
    ).toBeInTheDocument();
  });

  it("shows HUD banners for transcript notices", () => {
    const notices: DualViewNotice[] = [
      { level: "warn", message: "Polisher delayed", timestampMs: 10, frameIndex: 1 },
      { level: "info", message: "Local stream resumed", timestampMs: 11, frameIndex: 2 },
    ];

    const state = createTranscriptState({ notices });
    render(<DualViewPanel transcript={state} />);

    expect(screen.getByText("Polisher delayed")).toBeInTheDocument();
    expect(screen.getByText("Local stream resumed")).toBeInTheDocument();

    const warnBanner = screen.getByText("Polisher delayed").closest(
      ".dual-view-panel__banner",
    );
    expect(warnBanner).toHaveClass("dual-view-panel__banner--warn");
  });

  it("renders fallback messaging when an error occurs", () => {
    const state = createTranscriptState({
      error: "Bridge offline",
      sentences: [],
    });

    render(<DualViewPanel transcript={state} />);

    expect(
      screen.getByText(/we couldn't load transcript updates\. bridge offline/i),
    ).toBeInTheDocument();
    expect(
      screen.getAllByText(/transcript stream unavailable/i)[0],
    ).toBeInTheDocument();
  });

  it("keeps columns in sync when scrolling", () => {
    const sentences: DualViewSentence[] = Array.from({ length: 5 }).map(
      (_, index) => ({
        id: index + 1,
        firstFrameIndex: index,
        lastUpdated: Date.now(),
        activeVariant: "raw" as const,
        raw: buildVariant({ text: `raw ${index + 1}` }),
        polished: buildVariant({ text: `polished ${index + 1}`, source: "polished" }),
        pendingVariant: null,
        ariaLabel: `Sentence ${index + 1}`,
      }),
    );

    const state = createTranscriptState({ sentences, focusedSentenceId: 1 });
    render(<DualViewPanel transcript={state} />);

    const rawList = screen.getByRole("list", { name: /original transcript/i });
    const polishedList = screen.getByRole("list", { name: /polished transcript/i });

    Object.defineProperty(rawList, "scrollTop", {
      value: 0,
      writable: true,
    });
    Object.defineProperty(polishedList, "scrollTop", {
      value: 0,
      writable: true,
    });

    rawList.scrollTop = 120;
    fireEvent.scroll(rawList);

    expect(polishedList.scrollTop).toBe(120);
  });

  it("disables copy action until polished text is available", () => {
    const state = createTranscriptState();
    render(<DualViewPanel transcript={state} />);

    const copyButton = screen.getByRole("button", {
      name: /copy polished text/i,
    });
    expect(copyButton).toBeDisabled();
  });

  it("renders persisted publish notices when history is expanded", () => {
    const notices = [
      {
        sessionId: "session",
        action: "copy" as const,
        level: "warn" as const,
        message: "Clipboard fallback triggered",
        undoToken: null,
        timestampMs: Date.now(),
      },
      {
        sessionId: "session",
        action: "insert" as const,
        level: "info" as const,
        message: "Polished transcript inserted",
        undoToken: null,
        timestampMs: Date.now() + 1,
      },
    ];

    const state = createTranscriptState({ publishNotices: notices });
    render(<DualViewPanel transcript={state} />);

    const toggleHistory = screen.getByRole("button", { name: /show history/i });
    fireEvent.click(toggleHistory);

    expect(toggleHistory).toHaveTextContent(/hide history/i);
    expect(screen.getByRole("heading", { name: /recent notices/i })).toBeInTheDocument();
    expect(screen.getByText("Clipboard fallback triggered")).toBeInTheDocument();
    expect(screen.getByText("Polished transcript inserted")).toBeInTheDocument();
  });

  it("copies polished transcript content when requested", async () => {
    const sentence: DualViewSentence = {
      id: 8,
      firstFrameIndex: 0,
      lastUpdated: Date.now(),
      activeVariant: "polished",
      raw: buildVariant({ text: "raw" }),
      polished: buildVariant({ text: "polished body", source: "polished" }),
      pendingVariant: null,
      ariaLabel: "Sentence 8",
    };

    const writeText = vi.fn().mockResolvedValue(undefined);
    const navigatorWithClipboard = navigator as Navigator & {
      clipboard?: { writeText: (text: string) => Promise<void> };
    };
    const originalClipboard = navigatorWithClipboard.clipboard;
    Object.assign(navigatorWithClipboard, {
      clipboard: { writeText },
    });

    try {
      const state = createTranscriptState({ sentences: [sentence] });
      render(<DualViewPanel transcript={state} />);

      const copyButton = screen.getByRole("button", {
        name: /copy polished text/i,
      });
      expect(copyButton).toBeEnabled();

      await act(async () => {
        fireEvent.click(copyButton);
      });

      expect(writeText).toHaveBeenCalledWith("polished body");
    } finally {
      Object.assign(navigatorWithClipboard, { clipboard: originalClipboard });
    }
  });

  it("renders publishing failure details with retry messaging", () => {
    const sentence: DualViewSentence = {
      id: 12,
      firstFrameIndex: 0,
      lastUpdated: Date.now(),
      activeVariant: "polished",
      raw: buildVariant({ text: "raw text" }),
      polished: buildVariant({ text: "polished text", source: "polished" }),
      pendingVariant: null,
      ariaLabel: "Sentence 12",
    };

    const state = createTranscriptState({
      sentences: [sentence],
      publishUpdates: [
        {
          sessionId: "session-a",
          attempt: 1,
          strategy: "directInsert",
          fallback: null,
          retrying: false,
          detail: "Initial attempt",
          timestampMs: 10,
        },
        {
          sessionId: "session-a",
          attempt: 2,
          strategy: "clipboardFallback",
          fallback: "clipboardCopy",
          retrying: true,
          detail: "Focus lost, retrying",
          timestampMs: 20,
        },
      ],
      publishResults: [
        {
          sessionId: "session-a",
          status: "failed",
          strategy: "clipboardFallback",
          attempts: 2,
          fallback: "clipboardCopy",
          failure: {
            code: "focus-lost",
            message: "Target window lost focus",
          },
          undoToken: "undo-token",
          timestampMs: 30,
        },
      ],
      publishNotices: [
        {
          sessionId: "session-a",
          action: "undoPrompt",
          level: "warn",
          message: "Use Ctrl+Z to undo",
          undoToken: "undo-token",
          timestampMs: 40,
        },
      ],
    });

    render(<DualViewPanel transcript={state} />);

    expect(
      screen.getByText(/Automatic insertion failed after 2 attempt/i),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/Copied the transcript to your clipboard as a fallback/i),
    ).toBeInTheDocument();
    expect(screen.getByText(/Reason: Target window lost focus/i)).toBeInTheDocument();
    expect(screen.getByText(/Error code: focus-lost/i)).toBeInTheDocument();
    expect(
      screen.getByText(/Undo is available via Ctrl\/Cmd\+Z or from the clipboard backup/i),
    ).toBeInTheDocument();
    expect(screen.getByText(/Use Ctrl\+Z to undo/i)).toBeInTheDocument();
  });

  it("reveals publishing history and notices when toggled", () => {
    const state = createTranscriptState({
      publishUpdates: [
        {
          sessionId: "session-b",
          attempt: 1,
          strategy: "directInsert",
          fallback: null,
          retrying: false,
          detail: "Attempted direct insert",
          timestampMs: 5,
        },
        {
          sessionId: "session-b",
          attempt: 2,
          strategy: "clipboardFallback",
          fallback: "clipboardCopy",
          retrying: false,
          detail: "Copied to clipboard",
          timestampMs: 10,
        },
      ],
      publishResults: [
        {
          sessionId: "session-b",
          status: "deferred",
          strategy: "clipboardFallback",
          attempts: 2,
          fallback: "clipboardCopy",
          failure: {
            code: "timeout",
            message: "Manual paste required",
          },
          undoToken: null,
          timestampMs: 15,
        },
      ],
      publishNotices: [
        {
          sessionId: "session-b",
          action: "copy",
          level: "info",
          message: "Copied to clipboard",
          undoToken: null,
          timestampMs: 20,
        },
      ],
    });

    render(<DualViewPanel transcript={state} />);

    const toggleButton = screen.getByRole("button", { name: /show history/i });
    fireEvent.click(toggleButton);

    expect(screen.getByText(/Publishing attempts/i)).toBeInTheDocument();
    expect(screen.getByText(/Attempt 2/i)).toBeInTheDocument();
    expect(screen.getByText(/Fallback: Clipboard copy/i)).toBeInTheDocument();
    expect(screen.getByText(/Detail: Copied to clipboard/i)).toBeInTheDocument();
    expect(screen.getByText(/Recent notices/i)).toBeInTheDocument();
    expect(screen.getAllByText(/Copied to clipboard/i)).not.toHaveLength(0);
  });

  it("renders selection controls for polished sentences", async () => {
    const sentence: DualViewSentence = {
      id: 2,
      firstFrameIndex: 0,
      lastUpdated: Date.now(),
      activeVariant: "polished",
      raw: buildVariant({ text: "raw" }),
      polished: buildVariant({ text: "polished", source: "polished" }),
      pendingVariant: null,
      ariaLabel: "Sentence 2",
    };

    const toggleSelection = vi.fn();
    const markPendingSelection = vi.fn();
    const applySelection = vi.fn().mockResolvedValue(true);

    const state = createTranscriptState({
      sentences: [sentence],
      toggleSelection,
      markPendingSelection,
      applySelection,
    });

    render(<DualViewPanel transcript={state} />);

    const selectButton = screen.getByRole("button", {
      name: /select sentence 2/i,
    });
    fireEvent.click(selectButton);
    expect(toggleSelection).toHaveBeenCalledWith(2);

    const useOriginalButton = screen.getByRole("button", {
      name: /use original sentence 2/i,
    });
    await act(async () => {
      fireEvent.click(useOriginalButton);
    });

    expect(markPendingSelection).toHaveBeenCalledWith([2], "raw");
    expect(applySelection).toHaveBeenCalledWith([2], "raw");
  });

  it("shows batch revert toolbar when sentences are selected", async () => {
    const sentence: DualViewSentence = {
      id: 3,
      firstFrameIndex: 0,
      lastUpdated: Date.now(),
      activeVariant: "polished",
      raw: buildVariant({ text: "raw" }),
      polished: buildVariant({ text: "polished", source: "polished" }),
      pendingVariant: null,
      ariaLabel: "Sentence 3",
    };

    const markPendingSelection = vi.fn();
    const applySelection = vi.fn().mockResolvedValue(true);
    const clearSelections = vi.fn();

    const state = createTranscriptState({
      sentences: [sentence],
      selectedSentenceIds: [3],
      markPendingSelection,
      applySelection,
      clearSelections,
    });

    render(<DualViewPanel transcript={state} />);

    expect(screen.getByText(/1\/5 selected/i)).toBeInTheDocument();

    const revertButton = screen.getByRole("button", {
      name: /revert selected sentences to original/i,
    });
    expect(revertButton).toBeEnabled();

    await act(async () => {
      fireEvent.click(revertButton);
    });
    expect(markPendingSelection).toHaveBeenCalledWith([3], "raw");
    expect(applySelection).toHaveBeenCalledWith([3], "raw");

    const clearButton = screen.getByRole("button", { name: /clear/i });
    fireEvent.click(clearButton);
    expect(clearSelections).toHaveBeenCalled();
  });

  it("allows switching back to polished when raw is active", async () => {
    const sentence: DualViewSentence = {
      id: 4,
      firstFrameIndex: 0,
      lastUpdated: Date.now(),
      activeVariant: "raw",
      raw: buildVariant({ text: "raw" }),
      polished: buildVariant({ text: "polished", source: "polished" }),
      pendingVariant: null,
      ariaLabel: "Sentence 4",
    };

    const markPendingSelection = vi.fn();
    const applySelection = vi.fn().mockResolvedValue(true);

    const state = createTranscriptState({
      sentences: [sentence],
      markPendingSelection,
      applySelection,
    });

    render(<DualViewPanel transcript={state} />);

    const usePolishedButton = screen.getByRole("button", {
      name: /use polished sentence 4/i,
    });
    await act(async () => {
      fireEvent.click(usePolishedButton);
    });

    expect(markPendingSelection).toHaveBeenCalledWith([4], "polished");
    expect(applySelection).toHaveBeenCalledWith([4], "polished");
  });
});

