import { act, fireEvent, render, screen } from "@testing-library/react";
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

    expect(screen.getByText("raw sentence")).toBeInTheDocument();
    expect(screen.getByText("polished sentence")).toBeInTheDocument();
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

