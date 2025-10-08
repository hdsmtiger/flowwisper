import { invoke } from "@tauri-apps/api/core";

export type AccuracyFlag =
  | "accurate"
  | "inaccurate_raw"
  | "inaccurate_polished"
  | "unknown";

export type HistoryActionKind =
  | "copy"
  | "reinsert"
  | "export"
  | "save_draft"
  | "clipboard_backup";

export type HistoryPostAction = {
  kind: HistoryActionKind;
  timestampMs: number;
  detail: Record<string, unknown>;
};

export type HistoryEntry = {
  sessionId: string;
  startedAtMs: number;
  completedAtMs: number;
  durationMs: number;
  locale?: string | null;
  appIdentifier?: string | null;
  appVersion?: string | null;
  confidenceScore?: number | null;
  rawTranscript: string;
  polishedTranscript: string;
  preview: string;
  accuracyFlag: AccuracyFlag;
  accuracyRemarks?: string | null;
  postActions: HistoryPostAction[];
  metadata: Record<string, unknown>;
};

export type HistoryPage = {
  entries: HistoryEntry[];
  nextOffset: number | null;
  total: number | null;
};

export type HistoryQuery = {
  keyword?: string;
  locale?: string;
  appIdentifier?: string;
  limit?: number;
  offset?: number;
};

export type HistoryAccuracyRequest = {
  sessionId: string;
  flag: AccuracyFlag;
  remarks?: string;
};

export type HistoryActionRequest = {
  sessionId: string;
  action: HistoryActionKind;
  detail?: Record<string, unknown>;
};

const entryCache = new Map<string, HistoryEntry>();
const pageCache = new Map<string, HistoryPage>();

function invalidateSessionCache(sessionId: string) {
  entryCache.delete(sessionId);
  for (const [key, page] of pageCache.entries()) {
    if (page.entries.some((entry) => entry.sessionId === sessionId)) {
      pageCache.delete(key);
    }
  }
}

function cacheKey(query: HistoryQuery): string {
  const normalized: HistoryQuery = {
    keyword: query.keyword?.trim() || undefined,
    locale: query.locale?.trim() || undefined,
    appIdentifier: query.appIdentifier?.trim() || undefined,
    limit: query.limit ?? undefined,
    offset: query.offset ?? undefined,
  };
  return JSON.stringify(normalized);
}

function clone<T>(value: T): T {
  return structuredClone ? structuredClone(value) : JSON.parse(JSON.stringify(value));
}

export function clearHistoryCache() {
  entryCache.clear();
  pageCache.clear();
}

export async function searchHistory(query: HistoryQuery = {}): Promise<HistoryPage> {
  const key = cacheKey(query);
  if (pageCache.has(key)) {
    return clone(pageCache.get(key)!);
  }

  const page = await invoke<HistoryPage>("session_history_search", { query });
  page.entries.forEach((entry) => entryCache.set(entry.sessionId, entry));
  pageCache.set(key, page);
  return clone(page);
}

export async function loadHistoryEntry(sessionId: string): Promise<HistoryEntry | null> {
  if (entryCache.has(sessionId)) {
    return clone(entryCache.get(sessionId)!);
  }

  const entry = await invoke<HistoryEntry | null>("session_history_entry", { sessionId });
  if (entry) {
    entryCache.set(sessionId, entry);
    return clone(entry);
  }
  return null;
}

export async function markHistoryAccuracy(request: HistoryAccuracyRequest): Promise<void> {
  await invoke<void>("session_history_mark_accuracy", {
    update: request,
  });
  invalidateSessionCache(request.sessionId);
}

export async function recordHistoryAction(
  request: HistoryActionRequest,
): Promise<HistoryPostAction[]> {
  const actions = await invoke<HistoryPostAction[]>("session_history_append_action", {
    request,
  });
  invalidateSessionCache(request.sessionId);
  return clone(actions);
}
