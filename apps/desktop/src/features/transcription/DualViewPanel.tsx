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
  type PublishingUpdate,
  type InsertionResult,
  type PublishNotice,
  type PublishStatus,
  type PublishStrategy,
  type FallbackStrategy,
  MAX_MULTI_SELECT,
} from "./hooks/useDualViewTranscript";
import { useSessionEvents } from "./hooks/useSessionEvents";
import { NoiseBanner } from "./NoiseBanner";
import { SilenceCountdown } from "./SilenceCountdown";

import "./styles.css";

type ColumnLabels = Partial<Record<TranscriptSourceVariant, string>>;

type ScrollVariant = Extract<TranscriptSourceVariant, "raw" | "polished">;

const DEFAULT_LABELS: Record<ScrollVariant, string> = {
  raw: "Original transcript",
  polished: "Polished transcript",
};

const POLISHED_STYLE_LABEL = "Conversational tone · Light grammar fixes";

type Locale = "en" | "zh";

const LOCALE_MESSAGES: Record<Locale, {
  result: {
    title: Record<"idle" | "pending" | PublishStatus, string>;
    summary: Record<"idle" | "pending" | PublishStatus, string>;
    fallback: Record<FallbackStrategy | "none", string>;
    fallbackPrefix: string;
    detailPrefix: string;
    failureDetail: string;
    failureCode: string;
    polishedHeading: string;
    polishedPlaceholder: string;
    actions: {
      copy: string;
      historyShow: string;
      historyHide: string;
    };
    timelineHeading: string;
    timelineEmpty: string;
    noticesHeading: string;
    noticesEmpty: string;
    attemptLabel: string;
    strategyLabel: Record<PublishStrategy, string>;
    fallbackLabel: Record<FallbackStrategy | "none", string>;
    latestUpdateLabel: string;
    retrying: string;
    undoHint: string;
    clipboardHint: string;
    toast: {
      copySuccess: string;
      copyFailure: string;
    };
  };
}> = {
  en: {
    result: {
      title: {
        idle: "Polished transcript will be ready soon",
        pending: "Publishing polished transcript…",
        completed: "Polished transcript inserted",
        deferred: "Copied polished transcript to clipboard",
        failed: "Automatic insertion failed",
      },
      summary: {
        idle: "We’ll publish the polished transcript when the session ends.",
        pending: "We’re inserting the polished transcript automatically.",
        completed: "Inserted successfully after {attempts} attempt(s).",
        deferred: "Clipboard fallback triggered after {attempts} attempt(s). Paste manually if needed.",
        failed: "Automatic insertion failed after {attempts} attempt(s).",
      },
      fallback: {
        clipboardCopy: "Copied the transcript to your clipboard as a fallback.",
        notifyOnly: "Sent a desktop notice with the transcript.",
        none: "No fallback strategy applied.",
      },
      fallbackPrefix: "Fallback: {fallback}",
      detailPrefix: "Detail: {detail}",
      failureDetail: "Reason: {message}",
      failureCode: "Error code: {code}",
      polishedHeading: "Polished transcript",
      polishedPlaceholder: "Polished text will appear here after publishing.",
      actions: {
        copy: "Copy polished text",
        historyShow: "Show history",
        historyHide: "Hide history",
      },
      timelineHeading: "Publishing attempts",
      timelineEmpty: "No publishing attempts yet.",
      noticesHeading: "Recent notices",
      noticesEmpty: "No publishing notices yet.",
      attemptLabel: "Attempt {index}",
      strategyLabel: {
        directInsert: "Direct insert",
        clipboardFallback: "Clipboard fallback",
        notifyOnly: "Notify only",
      },
      fallbackLabel: {
        clipboardCopy: "Clipboard copy",
        notifyOnly: "Notify only",
        none: "None",
      },
      latestUpdateLabel: "Latest update",
      retrying: "Retrying…",
      undoHint: "Undo is available via Ctrl/Cmd+Z or from the clipboard backup.",
      clipboardHint: "Paste manually if automatic insertion fails.",
      toast: {
        copySuccess: "Polished transcript copied.",
        copyFailure: "Unable to copy polished transcript.",
      },
    },
  },
  zh: {
    result: {
      title: {
        idle: "润色稿即将生成",
        pending: "正在自动插入润色稿…",
        completed: "润色稿已自动插入",
        deferred: "已将润色稿复制到剪贴板",
        failed: "自动插入失败",
      },
      summary: {
        idle: "会话结束后将自动发布润色稿。",
        pending: "正在尝试自动插入润色后的文本。",
        completed: "{attempts} 次尝试后插入成功。",
        deferred: "{attempts} 次尝试后触发剪贴板降级，可手动粘贴。",
        failed: "{attempts} 次尝试后仍未插入成功。",
      },
      fallback: {
        clipboardCopy: "已将文本备份到剪贴板。",
        notifyOnly: "已发送桌面通知告知后续步骤。",
        none: "未使用降级策略。",
      },
      fallbackPrefix: "降级策略：{fallback}",
      detailPrefix: "详细信息：{detail}",
      failureDetail: "失败原因：{message}",
      failureCode: "错误码：{code}",
      polishedHeading: "润色稿",
      polishedPlaceholder: "润色内容准备完成后会显示在此。",
      actions: {
        copy: "复制润色稿",
        historyShow: "展开历史",
        historyHide: "收起历史",
      },
      timelineHeading: "发布尝试记录",
      timelineEmpty: "暂无发布尝试。",
      noticesHeading: "最新提示",
      noticesEmpty: "暂无发布提示。",
      attemptLabel: "第 {index} 次尝试",
      strategyLabel: {
        directInsert: "直接插入",
        clipboardFallback: "剪贴板降级",
        notifyOnly: "仅通知",
      },
      fallbackLabel: {
        clipboardCopy: "剪贴板备份",
        notifyOnly: "通知提醒",
        none: "无",
      },
      latestUpdateLabel: "最新动态",
      retrying: "正在重试…",
      undoHint: "可通过 Ctrl/Cmd+Z 或剪贴板备份撤销。",
      clipboardHint: "若插入失败，可手动粘贴到目标应用。",
      toast: {
        copySuccess: "已复制润色稿。",
        copyFailure: "复制润色稿失败。",
      },
    },
  },
};

const interpolate = (template: string, values: Record<string, string | number>): string =>
  template.replace(/\{(\w+)\}/g, (_, key) => {
    const replacement = values[key];
    return typeof replacement === "undefined" ? "" : String(replacement);
  });

const resolveLocale = (): Locale => {
  if (typeof navigator === "undefined") {
    return "en";
  }
  const language = navigator.language.toLowerCase();
  if (language.startsWith("zh")) {
    return "zh";
  }
  return "en";
};

type ResultMessages = (typeof LOCALE_MESSAGES)[Locale]["result"];

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

type ResultStatusKey = "idle" | "pending" | PublishStatus;

const STATUS_TONE: Record<ResultStatusKey, "info" | "success" | "warn" | "error"> = {
  idle: "info",
  pending: "info",
  completed: "success",
  deferred: "warn",
  failed: "error",
};

type ToastEntry = {
  id: string;
  message: string;
  level: PublishNotice["level"];
  visible: boolean;
};

const buildNoticeId = (notice: PublishNotice): string =>
  `${notice.timestampMs}-${notice.action}-${notice.message}`;

type ResultCardProps = {
  sentences: DualViewSentence[];
  updates: PublishingUpdate[];
  results: InsertionResult[];
  notices: PublishNotice[];
  messages: ResultMessages;
};

const ResultCard = ({
  sentences,
  updates = [],
  results = [],
  notices = [],
  messages,
}: ResultCardProps) => {
  const [historyOpen, setHistoryOpen] = useState(false);
  const [toasts, setToasts] = useState<ToastEntry[]>([]);
  const seenNoticeIds = useRef(new Set<string>());
  const hasHydratedNotices = useRef(false);

  const polishedParagraphs = useMemo(() => {
    return sentences
      .map((sentence) => sentence.polished?.text?.trim() || sentence.raw?.text?.trim() || "")
      .filter((text) => text.length > 0);
  }, [sentences]);

  const polishedText = useMemo(
    () => polishedParagraphs.join("\n\n"),
    [polishedParagraphs],
  );

  const latestResult = results.length > 0 ? results[results.length - 1] : null;
  const latestUpdate = updates.length > 0 ? updates[updates.length - 1] : null;
  const undoNotice = useMemo(() => {
    for (let index = notices.length - 1; index >= 0; index -= 1) {
      const notice = notices[index];
      if (notice.action === "undoPrompt") {
        return notice;
      }
    }
    return null;
  }, [notices]);

  const statusKey: ResultStatusKey = latestResult
    ? latestResult.status
    : latestUpdate
    ? "pending"
    : "idle";

  const attemptCount = latestResult?.attempts ?? updates.length;

  const statusLines = useMemo(() => {
    const lines: string[] = [];
    const summaryTemplate = messages.summary[statusKey];
    if (summaryTemplate) {
      lines.push(interpolate(summaryTemplate, { attempts: attemptCount }));
    }

    if (statusKey === "pending" && latestUpdate) {
      const attemptLabel = interpolate(messages.attemptLabel, {
        index: latestUpdate.attempt,
      });
      const strategyLabel =
        messages.strategyLabel[latestUpdate.strategy] || latestUpdate.strategy;
      lines.push(`${attemptLabel} · ${strategyLabel}`);
      if (latestUpdate.fallback) {
        lines.push(
          interpolate(messages.fallbackPrefix, {
            fallback:
              messages.fallbackLabel[latestUpdate.fallback] || latestUpdate.fallback,
          }),
        );
      }
      if (latestUpdate.retrying) {
        lines.push(messages.retrying);
      }
      if (latestUpdate.detail) {
        lines.push(
          interpolate(messages.detailPrefix, { detail: latestUpdate.detail }),
        );
      }
    }

    if (latestResult?.fallback) {
      lines.push(
        messages.fallback[latestResult.fallback] || latestResult.fallback,
      );
    }

    if (statusKey === "failed" && latestResult?.failure) {
      if (latestResult.failure.message) {
        lines.push(
          interpolate(messages.failureDetail, {
            message: latestResult.failure.message,
          }),
        );
      }
      if (latestResult.failure.code) {
        lines.push(
          interpolate(messages.failureCode, {
            code: latestResult.failure.code,
          }),
        );
      }
    }

    if (statusKey === "deferred" || statusKey === "failed") {
      lines.push(messages.clipboardHint);
    }

    if (undoNotice) {
      lines.push(messages.undoHint);
      if (undoNotice.message) {
        lines.push(undoNotice.message);
      }
    }

    return lines;
  }, [
    attemptCount,
    latestResult,
    latestUpdate,
    messages,
    statusKey,
    undoNotice,
  ]);

  const addToast = useCallback(
    (message: string, level: PublishNotice["level"] = "info", id?: string) => {
      const toastId = id ?? `${Date.now()}-${Math.random().toString(36).slice(2)}`;
      setToasts((current) => [
        ...current,
        {
          id: toastId,
          level,
          message,
          visible: true,
        },
      ]);
      const hideTimer = setTimeout(() => {
        setToasts((current) =>
          current.map((entry) =>
            entry.id === toastId ? { ...entry, visible: false } : entry,
          ),
        );
      }, 3000);
      const cleanupTimer = setTimeout(() => {
        setToasts((current) =>
          current.filter((entry) => entry.id !== toastId),
        );
      }, 3500);
      return () => {
        clearTimeout(hideTimer);
        clearTimeout(cleanupTimer);
      };
    },
    [],
  );

  useEffect(() => {
    if (!hasHydratedNotices.current) {
      notices.forEach((notice) => {
        seenNoticeIds.current.add(buildNoticeId(notice));
      });
      hasHydratedNotices.current = true;
      return;
    }

    notices.forEach((notice) => {
      const id = buildNoticeId(notice);
      if (seenNoticeIds.current.has(id)) {
        return;
      }
      seenNoticeIds.current.add(id);
      addToast(notice.message, notice.level, id);
    });
  }, [addToast, notices]);

  const handleCopy = useCallback(async () => {
    if (!polishedText) {
      return;
    }

    try {
      if (
        typeof navigator !== "undefined" &&
        navigator.clipboard &&
        typeof navigator.clipboard.writeText === "function"
      ) {
        await navigator.clipboard.writeText(polishedText);
      } else if (typeof document !== "undefined") {
        const textarea = document.createElement("textarea");
        textarea.value = polishedText;
        textarea.setAttribute("readonly", "");
        textarea.style.position = "absolute";
        textarea.style.left = "-9999px";
        document.body.appendChild(textarea);
        textarea.select();
        document.execCommand("copy");
        document.body.removeChild(textarea);
      }
      addToast(messages.toast.copySuccess, "info");
    } catch (error) {
      console.error("Failed to copy polished transcript", error);
      addToast(messages.toast.copyFailure, "error");
    }
  }, [addToast, messages.toast.copyFailure, messages.toast.copySuccess, polishedText]);

  const timelineItems = useMemo(() => {
    if (updates.length === 0) {
      return [] as PublishingUpdate[];
    }
    const limit = Math.min(10, updates.length);
    return updates.slice(updates.length - limit).reverse();
  }, [updates]);

  return (
    <section className="dual-view-panel__result-card" aria-live="polite">
      <header className="dual-view-panel__result-header">
        <div>
          <p
            className={`dual-view-panel__result-status dual-view-panel__result-status--${STATUS_TONE[statusKey]}`}
          >
            {messages.title[statusKey]}
          </p>
          <ul className="dual-view-panel__result-meta">
            {statusLines.map((line, index) => (
              <li key={`status-line-${index}`}>{line}</li>
            ))}
          </ul>
        </div>
        <div className="dual-view-panel__result-actions">
          <button
            type="button"
            className="dual-view-panel__action-button dual-view-panel__action-button--primary"
            onClick={() => {
              void handleCopy();
            }}
            disabled={!polishedText}
          >
            {messages.actions.copy}
          </button>
          <button
            type="button"
            className="dual-view-panel__action-button"
            onClick={() => {
              setHistoryOpen((current) => !current);
            }}
          >
            {historyOpen
              ? messages.actions.historyHide
              : messages.actions.historyShow}
          </button>
        </div>
      </header>
      <div className="dual-view-panel__result-body">
        <h3 className="dual-view-panel__result-heading">{messages.polishedHeading}</h3>
        <div className="dual-view-panel__result-content">
          {polishedText ? (
            <pre>{polishedText}</pre>
          ) : (
            <p className="dual-view-panel__result-placeholder">
              {messages.polishedPlaceholder}
            </p>
          )}
        </div>
      </div>
      {historyOpen ? (
        <div className="dual-view-panel__result-history">
          <div className="dual-view-panel__result-history-section">
            <h4>{messages.timelineHeading}</h4>
            {timelineItems.length === 0 ? (
              <p className="dual-view-panel__result-history-empty">
                {messages.timelineEmpty}
              </p>
            ) : (
              <ul>
                {timelineItems.map((update) => {
                  const strategyLabel =
                    messages.strategyLabel[update.strategy] || update.strategy;
                  const fallbackLabel = update.fallback
                    ? messages.fallbackLabel[update.fallback] || update.fallback
                    : messages.fallbackLabel.none;
                  return (
                    <li key={`${update.timestampMs}-${update.attempt}`}>
                      <div className="dual-view-panel__result-history-row">
                        <span className="dual-view-panel__result-history-attempt">
                          {interpolate(messages.attemptLabel, {
                            index: update.attempt,
                          })}
                        </span>
                        <span>{strategyLabel}</span>
                        <span>
                          {interpolate(messages.fallbackPrefix, {
                            fallback: fallbackLabel,
                          })}
                        </span>
                      </div>
                      {update.detail ? (
                        <p className="dual-view-panel__result-history-detail">
                          {interpolate(messages.detailPrefix, {
                            detail: update.detail,
                          })}
                        </p>
                      ) : null}
                    </li>
                  );
                })}
              </ul>
            )}
          </div>
          <div className="dual-view-panel__result-history-section">
            <h4>{messages.noticesHeading}</h4>
            {notices.length === 0 ? (
              <p className="dual-view-panel__result-history-empty">
                {messages.noticesEmpty}
              </p>
            ) : (
              <ul>
                {notices
                  .slice(Math.max(0, notices.length - 10))
                  .reverse()
                  .map((notice) => (
                    <li key={buildNoticeId(notice)}>
                      <div
                        className={`dual-view-panel__result-notice dual-view-panel__result-notice--${notice.level}`}
                      >
                        <span>{notice.message}</span>
                      </div>
                    </li>
                  ))}
              </ul>
            )}
          </div>
        </div>
      ) : null}
      <div className="dual-view-panel__result-toasts" aria-live="assertive">
        {toasts.map((toast) => (
          <div
            key={toast.id}
            role="status"
            className={`dual-view-panel__result-toast dual-view-panel__result-toast--${toast.level} ${
              toast.visible
                ? "dual-view-panel__result-toast--visible"
                : "dual-view-panel__result-toast--hidden"
            }`}
          >
            {toast.message}
          </div>
        ))}
      </div>
    </section>
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
    publishUpdates,
    publishResults,
    publishNotices,
  } = transcript;

  const sessionEvents = useSessionEvents();
  const overlayActive =
    sessionEvents.noiseWarning.visible ||
    sessionEvents.countdown.phase !== "idle" ||
    sessionEvents.autoStop.reason !== null;

  const locale = useMemo(() => resolveLocale(), []);
  const localeMessages = useMemo(() => LOCALE_MESSAGES[locale], [locale]);

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
      {overlayActive ? (
        <div className="transcription-overlay" role="presentation">
          <NoiseBanner
            warning={sessionEvents.noiseWarning}
            onDismiss={sessionEvents.dismissNoiseWarning}
          />
          <SilenceCountdown
            countdown={sessionEvents.countdown}
            autoStop={sessionEvents.autoStop}
            onDismissAutoStop={sessionEvents.resetAutoStop}
          />
        </div>
      ) : null}
      <ResultCard
        sentences={sentences}
        updates={publishUpdates}
        results={publishResults}
        notices={publishNotices}
        messages={localeMessages.result}
      />
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

