import {
  KeyboardEvent,
  ReactNode,
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";

import {
  type DualViewNotice,
  type DualViewSentence,
  type DualViewTranscriptState,
  type SentenceVariantState,
  type TranscriptSourceVariant,
  MAX_MULTI_SELECT,
} from "./hooks/useDualViewTranscript";

import "./styles.css";

type ColumnLabels = Partial<Record<TranscriptSourceVariant, string>>;

type ScrollVariant = Extract<TranscriptSourceVariant, "raw" | "polished">;

const DEFAULT_LABELS: Record<ScrollVariant, string> = {
  raw: "Original transcript",
  polished: "Polished transcript",
};

const POLISHED_STYLE_LABEL = "Conversational tone · Light grammar fixes";

const runOnNextFrame = (callback: () => void) => {
  if (
    typeof window !== "undefined" &&
    typeof window.requestAnimationFrame === "function"
  ) {
    window.requestAnimationFrame(callback);
    return;
  }
  callback();
};

const formatLatency = (latencyMs: number | undefined): string | null => {
  if (typeof latencyMs !== "number" || Number.isNaN(latencyMs)) {
    return null;
  }
  if (latencyMs >= 1000) {
    return `${(latencyMs / 1000).toFixed(1)}s`;
  }
  return `${Math.round(latencyMs)}ms`;
};

const describeSource = (state?: SentenceVariantState): string | null => {
  if (!state) {
    return null;
  }
  switch (state.source) {
    case "local":
      return "Local";
    case "cloud":
      return "Cloud";
    case "polished":
      return "Polisher";
    default:
      return null;
  }
};

type DualViewPanelProps = {
  transcript: DualViewTranscriptState;
  className?: string;
  columnLabels?: ColumnLabels;
  emptyState?: string;
  maxNotices?: number;
};

type BannerLevel = "info" | "warn" | "error";

type BannerEntry = {
  id: string;
  level: BannerLevel;
  message: string;
};

const bannerRole: Record<BannerLevel, "status" | "alert"> = {
  info: "status",
  warn: "alert",
  error: "alert",
};

const normalizeNotices = (
  notices: DualViewNotice[],
  limit: number,
): BannerEntry[] => {
  if (notices.length === 0) {
    return [];
  }
  const slice = limit > 0 ? notices.slice(-limit) : notices;
  return slice.map((notice) => ({
    id: `notice-${notice.timestampMs}-${notice.frameIndex}`,
    level: notice.level,
    message: notice.message,
  }));
};

type SentenceCardProps = {
  sentence: DualViewSentence;
  variant: ScrollVariant;
  isFocused: boolean;
  isSelected: boolean;
  onFocusSentence: (sentenceId: number) => void;
  onKeyDown?: (event: KeyboardEvent<HTMLDivElement>) => void;
  registerFocusRef?: (node: HTMLDivElement | null) => void;
  actions?: ReactNode;
};

const SentenceCard = ({
  sentence,
  variant,
  isFocused,
  isSelected,
  onFocusSentence,
  onKeyDown,
  registerFocusRef,
  actions,
}: SentenceCardProps) => {
  const variantState =
    variant === "raw" ? sentence.raw ?? null : sentence.polished ?? null;
  const isActive = sentence.activeVariant === variant;
  const isPending = sentence.pendingVariant === variant;
  const hasText = Boolean(variantState?.text?.trim());
  const placeholderText = variant === "polished"
    ? "Polishing…"
    : "Waiting for transcript";
  const showDelayWarning =
    variant === "polished" && Boolean(variantState) && !variantState.withinSla;
  const classes = [
    "dual-view-panel__sentence-card",
    `dual-view-panel__sentence-card--${variant}`,
  ];

  if (isActive) {
    classes.push("dual-view-panel__sentence-card--active");
  }
  if (isSelected) {
    classes.push("dual-view-panel__sentence-card--selected");
  }
  if (isPending) {
    classes.push("dual-view-panel__sentence-card--pending");
  }
  if (isFocused) {
    classes.push("dual-view-panel__sentence-card--focused");
  }
  if (!hasText) {
    classes.push("dual-view-panel__sentence-card--empty");
  }

  const badges: { label: string; tone: "info" | "warn" | "highlight" }[] = [];
  const sourceLabel = describeSource(variantState ?? undefined);
  if (sourceLabel) {
    badges.push({ label: sourceLabel, tone: "info" });
  }
  const latencyLabel = formatLatency(variantState?.latencyMs);
  if (latencyLabel) {
    badges.push({
      label: latencyLabel,
      tone: variantState?.withinSla ? "info" : "warn",
    });
  }
  if (isActive) {
    badges.push({ label: "Active", tone: "highlight" });
  }
  if (isPending) {
    badges.push({ label: "Pending", tone: "warn" });
  }

  const handleFocus = useCallback(() => {
    onFocusSentence(sentence.id);
  }, [onFocusSentence, sentence.id]);

  return (
    <div
      role="listitem"
      aria-label={sentence.ariaLabel}
      className={classes.join(" ")}
      data-variant={variant}
      tabIndex={isFocused ? 0 : -1}
      onFocus={handleFocus}
      onKeyDown={onKeyDown}
      ref={registerFocusRef}
    >
      <div className="dual-view-panel__sentence-header">
        <span className="dual-view-panel__sentence-title">
          Sentence {sentence.id}
        </span>
        {badges.length > 0 ? (
          <div className="dual-view-panel__badge-row">
            {badges.map((badge) => (
              <span
                key={`${badge.label}-${badge.tone}`}
                className={`dual-view-panel__badge dual-view-panel__badge--${badge.tone}`}
              >
                {badge.label}
              </span>
            ))}
          </div>
        ) : null}
      </div>
      {actions ? (
        <div className="dual-view-panel__sentence-actions">{actions}</div>
      ) : null}
      <p className="dual-view-panel__sentence-text">
        {hasText ? variantState!.text : placeholderText}
      </p>
      {showDelayWarning ? (
        <p className="dual-view-panel__sentence-warning">
          Polishing is taking longer than expected. You can continue waiting or
          use the original sentence.
        </p>
      ) : null}
    </div>
  );
};

export const DualViewPanel = ({
  transcript,
  className,
  columnLabels,
  emptyState = "Waiting for the transcription to begin",
  maxNotices = 3,
}: DualViewPanelProps) => {
  const {
    sentences,
    selectedSentenceIds,
    focusedSentenceId,
    focusSentence,
    focusNextSentence,
    focusPreviousSentence,
    toggleSelection,
    markPendingSelection,
    applySelection,
    pendingSelections,
    clearSelections,
    notices,
    error,
    isHydrated,
  } = transcript;

  const containerClass = useMemo(() => {
    const classes = ["dual-view-panel"];
    if (className) {
      classes.push(className);
    }
    return classes.join(" ");
  }, [className]);

  const selectedSet = useMemo(() => {
    if (selectedSentenceIds.length === 0) {
      return new Set<number>();
    }
    return new Set(selectedSentenceIds);
  }, [selectedSentenceIds]);

  const hasPendingInSelection = useMemo(
    () => selectedSentenceIds.some((id) => Boolean(pendingSelections[id])),
    [pendingSelections, selectedSentenceIds],
  );

  const [isBatchApplying, setIsBatchApplying] = useState(false);

  const rawScrollRef = useRef<HTMLDivElement | null>(null);
  const polishedScrollRef = useRef<HTMLDivElement | null>(null);
  const syncingRef = useRef(false);
  const focusRefs = useRef(new Map<number, HTMLDivElement>());

  const registerFocusRef = useCallback(
    (sentenceId: number) => (node: HTMLDivElement | null) => {
      if (!node) {
        focusRefs.current.delete(sentenceId);
        return;
      }
      focusRefs.current.set(sentenceId, node);
    },
    [],
  );

  const syncScrollPositions = useCallback(
    (source: ScrollVariant) => {
      const sourceRef = source === "raw" ? rawScrollRef : polishedScrollRef;
      const targetRef = source === "raw" ? polishedScrollRef : rawScrollRef;
      const sourceEl = sourceRef.current;
      const targetEl = targetRef.current;
      if (!sourceEl || !targetEl) {
        return;
      }
      syncingRef.current = true;
      targetEl.scrollTop = sourceEl.scrollTop;
      targetEl.scrollLeft = sourceEl.scrollLeft;
      runOnNextFrame(() => {
        syncingRef.current = false;
      });
    },
    [],
  );

  const handleRawScroll = useCallback(() => {
    if (syncingRef.current) {
      return;
    }
    syncScrollPositions("raw");
  }, [syncScrollPositions]);

  const handlePolishedScroll = useCallback(() => {
    if (syncingRef.current) {
      return;
    }
    syncScrollPositions("polished");
  }, [syncScrollPositions]);

  const handleSentenceKeyDown = useCallback(
    (event: KeyboardEvent<HTMLDivElement>) => {
      if (event.defaultPrevented) {
        return;
      }
      if (event.key === "ArrowDown") {
        event.preventDefault();
        focusNextSentence();
      } else if (event.key === "ArrowUp") {
        event.preventDefault();
        focusPreviousSentence();
      }
    },
    [focusNextSentence, focusPreviousSentence],
  );

  const handleToggleSelection = useCallback(
    (sentenceId: number) => {
      toggleSelection(sentenceId);
    },
    [toggleSelection],
  );

  const handleVariantChange = useCallback(
    async (sentenceId: number, targetVariant: TranscriptSourceVariant) => {
      markPendingSelection([sentenceId], targetVariant);
      const ok = await applySelection([sentenceId], targetVariant);
      if (ok && selectedSet.has(sentenceId)) {
        toggleSelection(sentenceId);
      }
    },
    [applySelection, markPendingSelection, selectedSet, toggleSelection],
  );

  const handleBatchRevert = useCallback(async () => {
    if (isBatchApplying) {
      return;
    }
    const ids = [...selectedSentenceIds];
    if (ids.length === 0) {
      return;
    }
    setIsBatchApplying(true);
    markPendingSelection(ids, "raw");
    await applySelection(ids, "raw");
    setIsBatchApplying(false);
  }, [
    applySelection,
    isBatchApplying,
    markPendingSelection,
    selectedSentenceIds,
  ]);

  useEffect(() => {
    if (focusedSentenceId === null) {
      return;
    }
    const node = focusRefs.current.get(focusedSentenceId);
    if (!node) {
      return;
    }
    node.focus({ preventScroll: true });
    if (typeof node.scrollIntoView === "function") {
      node.scrollIntoView({ block: "nearest" });
    }
    runOnNextFrame(() => {
      syncScrollPositions("raw");
    });
  }, [focusedSentenceId, sentences, syncScrollPositions]);

  const labels: Record<ScrollVariant, string> = useMemo(() => {
    return {
      raw: columnLabels?.raw ?? DEFAULT_LABELS.raw,
      polished: columnLabels?.polished ?? DEFAULT_LABELS.polished,
    };
  }, [columnLabels]);

  const selectedCount = selectedSentenceIds.length;
  const selectionSummary = `${selectedCount}/${MAX_MULTI_SELECT} selected`;
  const isBatchDisabled =
    isBatchApplying || selectedCount === 0 || hasPendingInSelection;

  const resolvedEmptyState = useMemo(() => {
    if (error) {
      return "Transcript stream unavailable. Try again or check your connection.";
    }
    if (!isHydrated) {
      return "Preparing transcript stream…";
    }
    return emptyState;
  }, [emptyState, error, isHydrated]);

  const bannerEntries = useMemo(() => {
    const entries: BannerEntry[] = [];
    if (error) {
      entries.push({
        id: "banner-error",
        level: "error",
        message: `We couldn't load transcript updates. ${error}`,
      });
    } else if (!isHydrated && sentences.length === 0) {
      entries.push({
        id: "banner-hydration",
        level: "info",
        message: "Connecting to the transcript service…",
      });
    }
    normalizeNotices(notices, maxNotices).forEach((entry) => {
      entries.push(entry);
    });
    return entries;
  }, [error, isHydrated, maxNotices, notices, sentences.length]);

  const hasBanners = bannerEntries.length > 0;

  return (
    <div className={containerClass}>
      {hasBanners ? (
        <div className="dual-view-panel__hud" role="presentation">
          {bannerEntries.map((banner) => (
            <div
              key={banner.id}
              role={bannerRole[banner.level]}
              className={`dual-view-panel__banner dual-view-panel__banner--${banner.level}`}
            >
              <span className="dual-view-panel__banner-indicator" aria-hidden="true" />
              <span className="dual-view-panel__banner-text">{banner.message}</span>
            </div>
          ))}
        </div>
      ) : null}
      <div className="dual-view-panel__columns">
        {(Object.keys(labels) as ScrollVariant[]).map((variant) => {
          const isRaw = variant === "raw";
          const listRef = isRaw ? rawScrollRef : polishedScrollRef;
          const onScroll = isRaw ? handleRawScroll : handlePolishedScroll;
          const ariaLabel = `${labels[variant]} sentences`;
          return (
          <section
            key={variant}
            className="dual-view-panel__column"
            data-variant={variant}
          >
            <header className="dual-view-panel__column-header">
              <div className="dual-view-panel__column-heading">
                <span className="dual-view-panel__column-title">
                  {labels[variant]}
                </span>
                {!isRaw ? (
                  <span className="dual-view-panel__column-style">
                    {POLISHED_STYLE_LABEL}
                  </span>
                ) : null}
              </div>
              <span className="dual-view-panel__column-counter">
                {sentences.length} sentences
              </span>
            </header>
            {!isRaw && selectedCount > 0 ? (
              <div className="dual-view-panel__selection-toolbar">
                <span className="dual-view-panel__selection-summary">
                  {selectionSummary}
                </span>
                <div className="dual-view-panel__selection-actions">
                  <button
                    type="button"
                    className="dual-view-panel__action-button dual-view-panel__action-button--primary"
                    onClick={() => {
                      void handleBatchRevert();
                    }}
                    disabled={isBatchDisabled}
                    aria-label="Revert selected sentences to original"
                  >
                    {isBatchApplying ? "Reverting…" : "Revert to original"}
                  </button>
                  <button
                    type="button"
                    className="dual-view-panel__action-button"
                    onClick={clearSelections}
                  >
                    Clear
                  </button>
                </div>
              </div>
            ) : null}
            <div
              className="dual-view-panel__scroll"
              role="list"
              aria-label={ariaLabel}
              ref={listRef}
              onScroll={onScroll}
            >
              {sentences.length === 0 ? (
                <p className="dual-view-panel__empty">{resolvedEmptyState}</p>
              ) : (
                sentences.map((sentence) => (
                  <SentenceCard
                    key={`${sentence.id}-${variant}`}
                    sentence={sentence}
                    variant={variant}
                    isFocused={isRaw && sentence.id === focusedSentenceId}
                    isSelected={selectedSet.has(sentence.id)}
                    onFocusSentence={focusSentence}
                    onKeyDown={isRaw ? handleSentenceKeyDown : undefined}
                    registerFocusRef={
                      isRaw ? registerFocusRef(sentence.id) : undefined
                    }
                    actions={
                      !isRaw
                        ? (
                            <>
                              <button
                                type="button"
                                className="dual-view-panel__action-chip"
                                aria-pressed={selectedSet.has(sentence.id)}
                                aria-label={`${
                                  selectedSet.has(sentence.id)
                                    ? "Deselect"
                                    : "Select"
                                } sentence ${sentence.id}`}
                                onClick={() => handleToggleSelection(sentence.id)}
                                disabled={
                                  !selectedSet.has(sentence.id) &&
                                  selectedCount >= MAX_MULTI_SELECT
                                }
                              >
                                {selectedSet.has(sentence.id)
                                  ? "Selected"
                                  : "Select"}
                              </button>
                              <button
                                type="button"
                                className="dual-view-panel__action-chip dual-view-panel__action-chip--ghost"
                                onClick={() =>
                                  void handleVariantChange(
                                    sentence.id,
                                    sentence.activeVariant === "polished"
                                      ? "raw"
                                      : "polished",
                                  )
                                }
                                disabled={
                                  sentence.pendingVariant !== null ||
                                  (sentence.activeVariant === "raw" &&
                                    !sentence.polished?.text) ||
                                  (sentence.activeVariant === "polished" &&
                                    !sentence.raw?.text)
                                }
                                aria-label={`${
                                  sentence.activeVariant === "polished"
                                    ? "Use original"
                                    : "Use polished"
                                } sentence ${sentence.id}`}
                              >
                                {sentence.pendingVariant ===
                                (sentence.activeVariant === "polished"
                                  ? "raw"
                                  : "polished")
                                  ? "Switching…"
                                  : sentence.activeVariant === "polished"
                                  ? "Use original"
                                  : "Use polished"}
                              </button>
                            </>
                          )
                        : undefined
                    }
                  />
                ))
              )}
            </div>
          </section>
          );
        })}
      </div>
    </div>
  );
};

export type { DualViewPanelProps };

