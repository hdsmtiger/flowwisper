import { useCallback, useEffect, useMemo, useReducer } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

const TRANSCRIPT_EVENT_CHANNEL = "session://transcript";
const LIFECYCLE_EVENT_CHANNEL = "session://lifecycle";
const PUBLISH_RESULT_CHANNEL = "session://publish-result";
const PUBLISH_NOTICE_CHANNEL = "session://publish-notice";
const TRANSCRIPT_LOG_COMMAND = "session_transcript_log";
const APPLY_SELECTION_COMMAND = "session_transcript_apply_selection";
const PUBLISH_HISTORY_COMMAND = "session_publish_history";
const PUBLISH_RESULTS_COMMAND = "session_publish_results";
const NOTICE_CENTER_COMMAND = "session_notice_center_history";
const MAX_NOTICE_HISTORY = 50;
const MAX_ANNOUNCEMENT_HISTORY = 25;
const MAX_PUBLISH_HISTORY = 120;
const MAX_PUBLISH_NOTICE_HISTORY = 80;
export const MAX_MULTI_SELECT = 5;

type AnnouncementPoliteness = "polite" | "assertive";

export type TranscriptSourceVariant = "raw" | "polished";

type TranscriptStreamSource = "local" | "cloud" | "polished";

type TranscriptNoticeLevel = "info" | "warn" | "error";

type TranscriptSentence = {
  sentenceId: number;
  text: string;
  source: TranscriptStreamSource;
  isPrimary: boolean;
  withinSla: boolean;
};

export type PublishStrategy =
  | "directInsert"
  | "clipboardFallback"
  | "notifyOnly";

export type FallbackStrategy = "clipboardCopy" | "notifyOnly";

export type PublishStatus = "completed" | "deferred" | "failed";

export type PublishNoticeLevel = "info" | "warn" | "error";

export type PublishActionKind =
  | "insert"
  | "copy"
  | "saveDraft"
  | "undoPrompt";

export type PublishingUpdate = {
  sessionId: string;
  attempt: number;
  strategy: PublishStrategy;
  fallback: FallbackStrategy | null;
  retrying: boolean;
  detail: string | null;
  timestampMs: number;
};

export type InsertionFailure = {
  code: string | null;
  message: string;
};

export type InsertionResult = {
  sessionId: string;
  status: PublishStatus;
  strategy: PublishStrategy;
  attempts: number;
  fallback: FallbackStrategy | null;
  failure: InsertionFailure | null;
  undoToken: string | null;
  timestampMs: number;
};

export type PublishNotice = {
  sessionId: string;
  action: PublishActionKind;
  level: PublishNoticeLevel;
  message: string;
  undoToken: string | null;
  timestampMs: number;
};

type TranscriptSentenceSelection = {
  sentenceId: number;
  activeVariant: TranscriptSourceVariant;
};

type TranscriptNotice = {
  level: TranscriptNoticeLevel;
  message: string;
};

type TranscriptStreamPayload =
  | {
      type: "transcript";
      sentence: TranscriptSentence;
    }
  | {
      type: "notice";
      notice: TranscriptNotice;
    }
  | {
      type: "selection";
      selections: TranscriptSentenceSelection[];
    };

type TranscriptStreamEvent = {
  timestampMs: number;
  frameIndex: number;
  latencyMs: number;
  isFirst: boolean;
  payload: TranscriptStreamPayload;
};

export type SentenceVariantState = {
  text: string;
  source: TranscriptStreamSource;
  latencyMs: number;
  withinSla: boolean;
  lastUpdated: number;
};

export type DualViewSentence = {
  id: number;
  firstFrameIndex: number;
  lastUpdated: number;
  activeVariant: TranscriptSourceVariant;
  raw?: SentenceVariantState;
  polished?: SentenceVariantState;
  pendingVariant: TranscriptSourceVariant | null;
  ariaLabel: string;
};

export type DualViewNotice = TranscriptNotice & {
  timestampMs: number;
  frameIndex: number;
};

export type DualViewAnnouncement = {
  id: string;
  message: string;
  politeness: AnnouncementPoliteness;
  timestampMs: number;
};

export type DualViewTranscriptState = {
  sentences: DualViewSentence[];
  notices: DualViewNotice[];
  selectedSentenceIds: number[];
  pendingSelections: Record<number, TranscriptSourceVariant>;
  isHydrated: boolean;
  error: string | null;
  focusedSentenceId: number | null;
  announcements: DualViewAnnouncement[];
  publishUpdates: PublishingUpdate[];
  publishResults: InsertionResult[];
  publishNotices: PublishNotice[];
  toggleSelection: (sentenceId: number) => void;
  selectSentences: (sentenceIds: number[]) => void;
  clearSelections: () => void;
  markPendingSelection: (
    sentenceIds: number[],
    targetVariant: TranscriptSourceVariant,
  ) => void;
  applySelection: (
    sentenceIds: number[],
    targetVariant: TranscriptSourceVariant,
  ) => Promise<boolean>;
  focusSentence: (sentenceId: number | null) => void;
  focusNextSentence: () => void;
  focusPreviousSentence: () => void;
  acknowledgeAnnouncement: (announcementId: string) => void;
};

type InternalSentenceRecord = {
  id: number;
  firstFrameIndex: number;
  lastUpdated: number;
  activeVariant: TranscriptSourceVariant;
  overrideVariant: TranscriptSourceVariant | null;
  raw?: SentenceVariantState;
  polished?: SentenceVariantState;
};

type InternalState = {
  sentences: Map<number, InternalSentenceRecord>;
  selected: Set<number>;
  pending: Map<number, TranscriptSourceVariant>;
  notices: DualViewNotice[];
  isHydrated: boolean;
  error: string | null;
  focusedSentenceId: number | null;
  announcements: DualViewAnnouncement[];
  publishUpdates: PublishingUpdate[];
  publishResults: InsertionResult[];
  publishNotices: PublishNotice[];
};

type Action =
  | { type: "hydrate"; events: TranscriptStreamEvent[] }
  | { type: "event"; event: TranscriptStreamEvent }
  | {
      type: "hydratePublishing";
      updates: PublishingUpdate[];
      results: InsertionResult[];
      notices: PublishNotice[];
    }
  | { type: "publishingUpdate"; update: PublishingUpdate }
  | { type: "publishResult"; result: InsertionResult }
  | { type: "publishNotice"; notice: PublishNotice }
  | { type: "toggleSelection"; sentenceId: number }
  | { type: "selectSentences"; sentenceIds: number[] }
  | { type: "clearSelections" }
  | {
      type: "markPending";
      sentenceIds: number[];
      targetVariant: TranscriptSourceVariant;
    }
  | {
      type: "selectionFailed";
      sentenceIds: number[];
      error: string;
    }
  | { type: "setError"; error: string }
  | { type: "markHydrated" }
  | { type: "focusSentence"; sentenceId: number | null }
  | { type: "focusRelative"; direction: 1 | -1 }
  | { type: "ackAnnouncement"; announcementId: string };

const initialState: InternalState = {
  sentences: new Map(),
  selected: new Set(),
  pending: new Map(),
  notices: [],
  isHydrated: false,
  error: null,
  focusedSentenceId: null,
  announcements: [],
  publishUpdates: [],
  publishResults: [],
  publishNotices: [],
};

let announcementCounter = 0;

const hasTauriBridge = (): boolean => {
  if (typeof window === "undefined") {
    return false;
  }
  const candidate = window as unknown as Record<string, unknown>;
  return (
    typeof candidate.__TAURI__ !== "undefined" ||
    typeof candidate.__TAURI_IPC__ !== "undefined"
  );
};

const cloneSentenceRecord = (
  record: InternalSentenceRecord,
): InternalSentenceRecord => ({
  id: record.id,
  firstFrameIndex: record.firstFrameIndex,
  lastUpdated: record.lastUpdated,
  activeVariant: record.activeVariant,
  overrideVariant: record.overrideVariant,
  raw: record.raw,
  polished: record.polished,
});

const formatSourceDescription = (source: TranscriptStreamSource): string => {
  switch (source) {
    case "local":
      return "local engine";
    case "cloud":
      return "cloud fallback";
    case "polished":
      return "polishing service";
    default: {
      const _never: never = source;
      return "unknown source";
    }
  }
};

const buildVariantDescription = (
  variantLabel: TranscriptSourceVariant,
  variant?: SentenceVariantState,
): string | null => {
  if (!variant) {
    return null;
  }
  const status = variant.withinSla ? "on time" : "delayed";
  const sourceDescription = formatSourceDescription(variant.source);
  const label = variantLabel === "polished" ? "Polished" : "Raw";
  return `${label} text ${status} from ${sourceDescription}.`;
};

const buildSentenceAriaLabel = (
  record: InternalSentenceRecord,
  pending: Map<number, TranscriptSourceVariant>,
): string => {
  const parts: string[] = [`Sentence ${record.id}.`];
  parts.push(
    record.activeVariant === "polished"
      ? "Polished version active."
      : "Raw version active.",
  );

  const rawDescription = buildVariantDescription("raw", record.raw);
  if (rawDescription) {
    parts.push(rawDescription);
  }

  const polishedDescription = buildVariantDescription(
    "polished",
    record.polished,
  );
  if (polishedDescription) {
    parts.push(polishedDescription);
  }

  const pendingVariant = pending.get(record.id);
  if (pendingVariant) {
    const variantLabel =
      pendingVariant === "polished" ? "polished" : "raw";
    parts.push(`Switching to ${variantLabel} pending.`);
  }

  return parts.join(" ");
};

const pushAnnouncement = (
  announcements: DualViewAnnouncement[],
  message: string,
  politeness: AnnouncementPoliteness,
  timestampMs: number,
): DualViewAnnouncement[] => {
  const entry: DualViewAnnouncement = {
    id: `${timestampMs}-${announcementCounter++}`,
    message,
    politeness,
    timestampMs,
  };
  const next = [...announcements, entry];
  if (next.length > MAX_ANNOUNCEMENT_HISTORY) {
    return next.slice(next.length - MAX_ANNOUNCEMENT_HISTORY);
  }
  return next;
};

const describePendingSwitch = (
  sentenceIds: number[],
  targetVariant: TranscriptSourceVariant,
): string => {
  if (sentenceIds.length === 1) {
    return `Pending switch for sentence ${sentenceIds[0]} to ${targetVariant}.`;
  }
  return `Pending switch for ${sentenceIds.length} sentences to ${targetVariant}.`;
};

const describeSelectionLimit = (limit: number): string =>
  `You can select up to ${limit} sentences at once.`;

const describeSelectionFailure = (
  sentenceIds: number[],
  error: string,
): string => {
  const base =
    sentenceIds.length === 1
      ? `Failed to update sentence ${sentenceIds[0]} selection.`
      : `Failed to update ${sentenceIds.length} selections.`;
  return `${base} ${error}`.trim();
};

const orderedSentenceIds = (
  sentences: Map<number, InternalSentenceRecord>,
): number[] => {
  const records = Array.from(sentences.values());
  records.sort((a, b) => {
    if (a.firstFrameIndex === b.firstFrameIndex) {
      return a.id - b.id;
    }
    return a.firstFrameIndex - b.firstFrameIndex;
  });
  return records.map((record) => record.id);
};

const reduceWithEvent = (
  state: InternalState,
  event: TranscriptStreamEvent,
): InternalState => {
  let baseState = state;

  if (event.isFirst && event.frameIndex === 0) {
    baseState = {
      sentences: new Map(),
      selected: new Set(),
      pending: new Map(),
      notices: [],
      isHydrated: state.isHydrated,
      error: state.error,
      focusedSentenceId: null,
      announcements: [],
    };
  }

  const sentences = new Map(baseState.sentences);
  const selected = new Set(baseState.selected);
  const pending = new Map(baseState.pending);
  let notices = baseState.notices;
  let announcements = baseState.announcements;
  let focusedSentenceId = baseState.focusedSentenceId;

  switch (event.payload.type) {
    case "transcript": {
      const { sentence } = event.payload;
      const sentenceId = sentence.sentenceId;
      const variant: TranscriptSourceVariant =
        sentence.source === "polished" ? "polished" : "raw";
      const didExist = sentences.has(sentenceId);
      const nextRecord: InternalSentenceRecord = didExist
        ? cloneSentenceRecord(sentences.get(sentenceId)!)
        : {
            id: sentenceId,
            firstFrameIndex: event.frameIndex,
            lastUpdated: event.timestampMs,
            activeVariant: variant,
            overrideVariant: null,
          };

      const nextVariant: SentenceVariantState = {
        text: sentence.text,
        source: sentence.source,
        latencyMs: event.latencyMs,
        withinSla: sentence.withinSla,
        lastUpdated: event.timestampMs,
      };

      if (variant === "raw") {
        nextRecord.raw = nextVariant;
      } else {
        nextRecord.polished = nextVariant;
      }

      if (sentence.isPrimary) {
        if (
          nextRecord.overrideVariant === null ||
          nextRecord.overrideVariant === variant
        ) {
          nextRecord.activeVariant = variant;
          if (variant === "polished") {
            nextRecord.overrideVariant = null;
          }
        }
      }

      if (nextRecord.overrideVariant) {
        nextRecord.activeVariant = nextRecord.overrideVariant;
      }

      nextRecord.lastUpdated = event.timestampMs;
      sentences.set(sentenceId, nextRecord);

      if (!didExist && focusedSentenceId === null) {
        focusedSentenceId = sentenceId;
      }

      const latencyLabel = sentence.withinSla ? "on time" : "delayed";
      const variantLabel = variant === "polished" ? "polished" : "raw";
      announcements = pushAnnouncement(
        announcements,
        `Sentence ${sentenceId} ${variantLabel} update ${latencyLabel}.`,
        "polite",
        event.timestampMs,
      );
      break;
    }
    case "notice": {
      const entry: DualViewNotice = {
        ...event.payload.notice,
        timestampMs: event.timestampMs,
        frameIndex: event.frameIndex,
      };
      notices = [...baseState.notices, entry];
      if (notices.length > MAX_NOTICE_HISTORY) {
        notices = notices.slice(notices.length - MAX_NOTICE_HISTORY);
      }
      const politeness: AnnouncementPoliteness =
        event.payload.notice.level === "error" ? "assertive" : "polite";
      announcements = pushAnnouncement(
        announcements,
        event.payload.notice.message,
        politeness,
        event.timestampMs,
      );
      break;
    }
    case "selection": {
      if (event.payload.selections.length === 0) {
        break;
      }
      const updates = event.payload.selections;
      updates.forEach((selection) => {
        const sentenceId = selection.sentenceId;
        const variant = selection.activeVariant;
        const record: InternalSentenceRecord = sentences.has(sentenceId)
          ? cloneSentenceRecord(sentences.get(sentenceId)!)
          : {
              id: sentenceId,
              firstFrameIndex: event.frameIndex,
              lastUpdated: event.timestampMs,
              activeVariant: variant,
              overrideVariant: null,
            };
        record.overrideVariant = variant === "raw" ? "raw" : null;
        record.activeVariant = variant;
        record.lastUpdated = event.timestampMs;
        sentences.set(sentenceId, record);
        pending.delete(sentenceId);
        selected.delete(sentenceId);
        announcements = pushAnnouncement(
          announcements,
          `Sentence ${sentenceId} active variant set to ${variant}.`,
          "polite",
          event.timestampMs,
        );
      });
      break;
    }
    default: {
      const _exhaustiveCheck: never = event.payload;
      return baseState;
    }
  }

  return {
    sentences,
    selected,
    pending,
    notices,
    isHydrated: baseState.isHydrated,
    error: baseState.error,
    focusedSentenceId,
    announcements,
    publishUpdates: baseState.publishUpdates,
    publishResults: baseState.publishResults,
    publishNotices: baseState.publishNotices,
  };
};

const reducer = (state: InternalState, action: Action): InternalState => {
  switch (action.type) {
    case "hydrate": {
      let nextState = state;
      action.events.forEach((event) => {
        nextState = reduceWithEvent(nextState, event);
      });
      return {
        ...nextState,
        isHydrated: true,
      };
    }
    case "event": {
      const nextState = reduceWithEvent(state, action.event);
      return {
        ...nextState,
        isHydrated: true,
      };
    }
    case "hydratePublishing": {
      const updates = action.updates.slice(-MAX_PUBLISH_HISTORY);
      const results = action.results.slice(-MAX_PUBLISH_HISTORY);
      const notices = action.notices.slice(-MAX_PUBLISH_NOTICE_HISTORY);
      return {
        ...state,
        publishUpdates: updates,
        publishResults: results,
        publishNotices: notices,
      };
    }
    case "publishingUpdate": {
      const updates = [...state.publishUpdates, action.update];
      if (updates.length > MAX_PUBLISH_HISTORY) {
        updates.splice(0, updates.length - MAX_PUBLISH_HISTORY);
      }
      return {
        ...state,
        publishUpdates: updates,
      };
    }
    case "publishResult": {
      const results = [...state.publishResults, action.result];
      if (results.length > MAX_PUBLISH_HISTORY) {
        results.splice(0, results.length - MAX_PUBLISH_HISTORY);
      }
      return {
        ...state,
        publishResults: results,
      };
    }
    case "publishNotice": {
      const notices = [...state.publishNotices, action.notice];
      if (notices.length > MAX_PUBLISH_NOTICE_HISTORY) {
        notices.splice(0, notices.length - MAX_PUBLISH_NOTICE_HISTORY);
      }
      return {
        ...state,
        publishNotices: notices,
      };
    }
    case "toggleSelection": {
      const selected = new Set(state.selected);
      let announcements = state.announcements;
      if (selected.has(action.sentenceId)) {
        selected.delete(action.sentenceId);
      } else {
        if (selected.size >= MAX_MULTI_SELECT) {
          announcements = pushAnnouncement(
            announcements,
            describeSelectionLimit(MAX_MULTI_SELECT),
            "assertive",
            Date.now(),
          );
          return {
            ...state,
            announcements,
          };
        }
        selected.add(action.sentenceId);
      }
      return {
        ...state,
        selected,
        announcements,
      };
    }
    case "selectSentences": {
      if (action.sentenceIds.length === 0) {
        return state;
      }
      const selected = new Set(state.selected);
      let remainingSlots = MAX_MULTI_SELECT - selected.size;
      let announcements = state.announcements;
      for (const id of action.sentenceIds) {
        if (selected.has(id)) {
          continue;
        }
        if (remainingSlots <= 0) {
          announcements = pushAnnouncement(
            announcements,
            describeSelectionLimit(MAX_MULTI_SELECT),
            "assertive",
            Date.now(),
          );
          break;
        }
        selected.add(id);
        remainingSlots -= 1;
      }
      return {
        ...state,
        selected,
        announcements,
      };
    }
    case "clearSelections": {
      if (state.selected.size === 0 && state.pending.size === 0) {
        return state;
      }
      return {
        ...state,
        selected: new Set(),
        pending: new Map(),
      };
    }
    case "markPending": {
      if (action.sentenceIds.length === 0) {
        return state;
      }
      const pending = new Map(state.pending);
      action.sentenceIds.forEach((id) => {
        pending.set(id, action.targetVariant);
      });
      const announcements = pushAnnouncement(
        state.announcements,
        describePendingSwitch(action.sentenceIds, action.targetVariant),
        "polite",
        Date.now(),
      );
      return {
        ...state,
        pending,
        announcements,
      };
    }
    case "selectionFailed": {
      if (action.sentenceIds.length === 0) {
        return state;
      }
      const pending = new Map(state.pending);
      let removed = false;
      action.sentenceIds.forEach((id) => {
        if (pending.delete(id)) {
          removed = true;
        }
      });
      const announcements = pushAnnouncement(
        state.announcements,
        describeSelectionFailure(action.sentenceIds, action.error),
        "assertive",
        Date.now(),
      );
      if (!removed && announcements === state.announcements) {
        return state;
      }
      return {
        ...state,
        pending,
        announcements,
      };
    }
    case "setError": {
      const announcements = pushAnnouncement(
        state.announcements,
        `Transcript stream error: ${action.error}.`,
        "assertive",
        Date.now(),
      );
      return {
        ...state,
        error: action.error,
        isHydrated: true,
        announcements,
      };
    }
    case "markHydrated": {
      if (state.isHydrated) {
        return state;
      }
      return {
        ...state,
        isHydrated: true,
      };
    }
    case "focusSentence": {
      if (action.sentenceId === null) {
        if (state.focusedSentenceId === null) {
          return state;
        }
        return {
          ...state,
          focusedSentenceId: null,
        };
      }
      if (!state.sentences.has(action.sentenceId)) {
        return state;
      }
      if (state.focusedSentenceId === action.sentenceId) {
        return state;
      }
      return {
        ...state,
        focusedSentenceId: action.sentenceId,
      };
    }
    case "focusRelative": {
      if (state.sentences.size === 0) {
        return state;
      }
      const ids = orderedSentenceIds(state.sentences);
      if (ids.length === 0) {
        return state;
      }
      if (state.focusedSentenceId === null) {
        const initialId = action.direction === -1 ? ids[ids.length - 1] : ids[0];
        return {
          ...state,
          focusedSentenceId: initialId,
        };
      }
      const currentIndex = ids.indexOf(state.focusedSentenceId);
      if (currentIndex === -1) {
        return {
          ...state,
          focusedSentenceId: ids[0],
        };
      }
      let nextIndex = currentIndex + action.direction;
      if (nextIndex < 0) {
        nextIndex = 0;
      }
      if (nextIndex >= ids.length) {
        nextIndex = ids.length - 1;
      }
      const nextId = ids[nextIndex];
      if (nextId === state.focusedSentenceId) {
        return state;
      }
      return {
        ...state,
        focusedSentenceId: nextId,
      };
    }
    case "ackAnnouncement": {
      if (state.announcements.length === 0) {
        return state;
      }
      const announcements = state.announcements.filter(
        (entry) => entry.id !== action.announcementId,
      );
      if (announcements.length === state.announcements.length) {
        return state;
      }
      return {
        ...state,
        announcements,
      };
    }
    default: {
      const _never: never = action;
      return state;
    }
  }
};

const toDualViewSentence = (
  record: InternalSentenceRecord,
  pending: Map<number, TranscriptSourceVariant>,
): DualViewSentence => ({
  id: record.id,
  firstFrameIndex: record.firstFrameIndex,
  lastUpdated: record.lastUpdated,
  activeVariant: record.activeVariant,
  raw: record.raw,
  polished: record.polished,
  pendingVariant: pending.get(record.id) ?? null,
  ariaLabel: buildSentenceAriaLabel(record, pending),
});

const buildPendingLookup = (
  pending: Map<number, TranscriptSourceVariant>,
): Record<number, TranscriptSourceVariant> => {
  if (pending.size === 0) {
    return {};
  }
  const entries = Array.from(pending.entries()).sort((a, b) => a[0] - b[0]);
  return entries.reduce<Record<number, TranscriptSourceVariant>>(
    (acc, [id, variant]) => {
      acc[id] = variant;
      return acc;
    },
    {},
  );
};

export const useDualViewTranscript = (): DualViewTranscriptState => {
  const [state, dispatch] = useReducer(reducer, initialState);

  useEffect(() => {
    if (!hasTauriBridge()) {
      dispatch({ type: "markHydrated" });
      return;
    }

    let active = true;
    const unlisteners: UnlistenFn[] = [];

    (async () => {
      try {
        const [
          transcriptHistory,
          publishHistory,
          publishResults,
          publishNotices,
        ] = await Promise.all([
          invoke<TranscriptStreamEvent[]>(TRANSCRIPT_LOG_COMMAND),
          invoke<PublishingUpdate[]>(PUBLISH_HISTORY_COMMAND),
          invoke<InsertionResult[]>(PUBLISH_RESULTS_COMMAND),
          invoke<PublishNotice[]>(NOTICE_CENTER_COMMAND),
        ]);

        if (active) {
          if (Array.isArray(transcriptHistory)) {
            dispatch({ type: "hydrate", events: transcriptHistory });
          }
          dispatch({
            type: "hydratePublishing",
            updates: Array.isArray(publishHistory) ? publishHistory : [],
            results: Array.isArray(publishResults) ? publishResults : [],
            notices: Array.isArray(publishNotices) ? publishNotices : [],
          });
        }
      } catch (error) {
        if (active) {
          dispatch({ type: "setError", error: String(error) });
        }
      }

      const registerListener = async <T,>(
        channel: string,
        handler: (payload: T) => void,
      ) => {
        try {
          const stop = await listen<T>(channel, (event) => {
            handler(event.payload);
          });
          if (active) {
            unlisteners.push(stop);
          } else {
            stop();
          }
        } catch (error) {
          if (active) {
            dispatch({ type: "setError", error: String(error) });
          }
        }
      };

      await registerListener<TranscriptStreamEvent>(
        TRANSCRIPT_EVENT_CHANNEL,
        (event) => {
          dispatch({ type: "event", event });
        },
      );

      await registerListener<PublishingUpdate>(
        LIFECYCLE_EVENT_CHANNEL,
        (update) => {
          dispatch({ type: "publishingUpdate", update });
        },
      );

      await registerListener<InsertionResult>(
        PUBLISH_RESULT_CHANNEL,
        (result) => {
          dispatch({ type: "publishResult", result });
        },
      );

      await registerListener<PublishNotice>(
        PUBLISH_NOTICE_CHANNEL,
        (notice) => {
          dispatch({ type: "publishNotice", notice });
        },
      );
    })();

    return () => {
      active = false;
      unlisteners.splice(0).forEach((stop) => {
        try {
          stop();
        } catch (error) {
          console.warn("Failed to cleanup listener", error);
        }
      });
    };
  }, []);

  const sentences = useMemo(() => {
    const items = Array.from(state.sentences.values());
    items.sort((a, b) => {
      if (a.firstFrameIndex === b.firstFrameIndex) {
        return a.id - b.id;
      }
      return a.firstFrameIndex - b.firstFrameIndex;
    });
    return items.map((record) => toDualViewSentence(record, state.pending));
  }, [state.sentences, state.pending]);

  const notices = useMemo(() => state.notices, [state.notices]);

  const selectedSentenceIds = useMemo(() => {
    const ids = Array.from(state.selected.values());
    ids.sort((a, b) => a - b);
    return ids;
  }, [state.selected]);

  const pendingSelections = useMemo(
    () => buildPendingLookup(state.pending),
    [state.pending],
  );

  const announcements = useMemo(
    () => state.announcements,
    [state.announcements],
  );

  const toggleSelection = useCallback((sentenceId: number) => {
    dispatch({ type: "toggleSelection", sentenceId });
  }, []);

  const selectSentences = useCallback((sentenceIds: number[]) => {
    dispatch({ type: "selectSentences", sentenceIds });
  }, []);

  const clearSelections = useCallback(() => {
    dispatch({ type: "clearSelections" });
  }, []);

  const markPendingSelection = useCallback(
    (sentenceIds: number[], targetVariant: TranscriptSourceVariant) => {
      dispatch({ type: "markPending", sentenceIds, targetVariant });
    },
    [],
  );

  const applySelection = useCallback(
    async (sentenceIds: number[], targetVariant: TranscriptSourceVariant) => {
      if (sentenceIds.length === 0) {
        return true;
      }

      if (!hasTauriBridge()) {
        return true;
      }

      try {
        await invoke(APPLY_SELECTION_COMMAND, {
          selections: sentenceIds.map((id) => ({
            sentenceId: id,
            activeVariant: targetVariant,
          })),
        });
        return true;
      } catch (error) {
        dispatch({
          type: "selectionFailed",
          sentenceIds,
          error: String(error),
        });
        return false;
      }
    },
    [],
  );

  const focusSentence = useCallback((sentenceId: number | null) => {
    dispatch({ type: "focusSentence", sentenceId });
  }, []);

  const focusNextSentence = useCallback(() => {
    dispatch({ type: "focusRelative", direction: 1 });
  }, []);

  const focusPreviousSentence = useCallback(() => {
    dispatch({ type: "focusRelative", direction: -1 });
  }, []);

  const acknowledgeAnnouncement = useCallback((announcementId: string) => {
    dispatch({ type: "ackAnnouncement", announcementId });
  }, []);

  return {
    sentences,
    notices,
    selectedSentenceIds,
    pendingSelections,
    isHydrated: state.isHydrated,
    error: state.error,
    focusedSentenceId: state.focusedSentenceId,
    announcements,
    publishUpdates: state.publishUpdates,
    publishResults: state.publishResults,
    publishNotices: state.publishNotices,
    toggleSelection,
    selectSentences,
    clearSelections,
    markPendingSelection,
    applySelection,
    focusSentence,
    focusNextSentence,
    focusPreviousSentence,
    acknowledgeAnnouncement,
  };
};

