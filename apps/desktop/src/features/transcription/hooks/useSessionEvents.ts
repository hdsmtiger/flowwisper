import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen, type UnlistenFn } from "@tauri-apps/api/event";

const SESSION_EVENT_CHANNEL = "session://event";
const SESSION_EVENT_HISTORY_COMMAND = "session_event_history";
const NOISE_DISMISS_DELAY_MS = 4000;

type SessionSilenceCountdownState = "started" | "tick" | "canceled" | "completed";
type SessionSilenceCancellationReason = "speechDetected" | "manualStop";
type SessionAutoStopReason = "silenceTimeout";

type SessionEventPayload =
  | {
      type: "noiseWarning";
      timestampMs: number;
      baselineDb: number;
      thresholdDb: number;
      levelDb: number;
      persistenceMs: number;
    }
  | {
      type: "silenceCountdown";
      timestampMs: number;
      totalMs: number;
      remainingMs: number;
      state: SessionSilenceCountdownState;
      cancelReason?: SessionSilenceCancellationReason | null;
    }
  | {
      type: "autoStop";
      timestampMs: number;
      reason: SessionAutoStopReason;
    };

export type NoiseWarningState = {
  visible: boolean;
  baselineDb: number;
  thresholdDb: number;
  levelDb: number;
  persistenceMs: number;
  triggeredAt: number;
};

export type CountdownState = {
  phase: SessionSilenceCountdownState | "idle";
  totalMs: number;
  remainingMs: number;
  cancelReason: SessionSilenceCancellationReason | null;
  updatedAt: number;
};

export type AutoStopState = {
  reason: SessionAutoStopReason | null;
  timestampMs: number | null;
};

export type SessionEventsState = {
  noiseWarning: NoiseWarningState;
  dismissNoiseWarning: () => void;
  countdown: CountdownState;
  autoStop: AutoStopState;
  resetAutoStop: () => void;
};

const createInitialNoiseState = (): NoiseWarningState => ({
  visible: false,
  baselineDb: 0,
  thresholdDb: 0,
  levelDb: 0,
  persistenceMs: 0,
  triggeredAt: 0,
});

const createInitialCountdownState = (): CountdownState => ({
  phase: "idle",
  totalMs: 5000,
  remainingMs: 0,
  cancelReason: null,
  updatedAt: 0,
});

const createInitialAutoStopState = (): AutoStopState => ({
  reason: null,
  timestampMs: null,
});

const isNoiseEvent = (event: SessionEventPayload): event is Extract<SessionEventPayload, { type: "noiseWarning" }> =>
  event.type === "noiseWarning";

const isCountdownEvent = (
  event: SessionEventPayload,
): event is Extract<SessionEventPayload, { type: "silenceCountdown" }> => event.type === "silenceCountdown";

const isAutoStopEvent = (event: SessionEventPayload): event is Extract<SessionEventPayload, { type: "autoStop" }> =>
  event.type === "autoStop";

const coerceEventPayload = (payload: unknown): SessionEventPayload | null => {
  if (!payload || typeof payload !== "object") {
    return null;
  }

  const record = payload as Record<string, unknown>;
  const type = record["type"];

  if (type === "noiseWarning") {
    const baseline = Number(record["baselineDb"]);
    const threshold = Number(record["thresholdDb"]);
    const level = Number(record["levelDb"]);
    const persistence = Number(record["persistenceMs"]);
    const timestamp = Number(record["timestampMs"]);
    if ([baseline, threshold, level, persistence, timestamp].some((value) => Number.isNaN(value))) {
      return null;
    }
    return {
      type: "noiseWarning",
      timestampMs: timestamp,
      baselineDb: baseline,
      thresholdDb: threshold,
      levelDb: level,
      persistenceMs: persistence,
    };
  }

  if (type === "silenceCountdown") {
    const timestamp = Number(record["timestampMs"]);
    const total = Number(record["totalMs"]);
    const remaining = Number(record["remainingMs"]);
    const state = record["state"];
    const cancelReason = record["cancelReason"];
    if (
      [timestamp, total, remaining].some((value) => Number.isNaN(value)) ||
      typeof state !== "string"
    ) {
      return null;
    }
    let reason: SessionSilenceCancellationReason | null = null;
    if (typeof cancelReason === "string") {
      if (cancelReason === "speechDetected" || cancelReason === "manualStop") {
        reason = cancelReason;
      } else {
        return null;
      }
    }
    return {
      type: "silenceCountdown",
      timestampMs: timestamp,
      totalMs: total,
      remainingMs: remaining,
      state: state as SessionSilenceCountdownState,
      cancelReason: reason,
    };
  }

  if (type === "autoStop") {
    const timestamp = Number(record["timestampMs"]);
    const reason = record["reason"];
    if (Number.isNaN(timestamp) || reason !== "silenceTimeout") {
      return null;
    }
    return {
      type: "autoStop",
      timestampMs: timestamp,
      reason: "silenceTimeout",
    };
  }

  return null;
};

export const useSessionEvents = (): SessionEventsState => {
  const [noiseWarning, setNoiseWarning] = useState<NoiseWarningState>(() => createInitialNoiseState());
  const [countdown, setCountdown] = useState<CountdownState>(() => createInitialCountdownState());
  const [autoStop, setAutoStop] = useState<AutoStopState>(() => createInitialAutoStopState());
  const lastProcessedTimestamp = useRef<number>(0);

  const dismissNoiseWarning = useCallback(() => {
    setNoiseWarning((current) => ({ ...current, visible: false }));
  }, []);

  const resetAutoStop = useCallback(() => {
    setAutoStop(createInitialAutoStopState());
  }, []);

  const applyEvent = useCallback((event: SessionEventPayload) => {
    if (event.timestampMs && event.timestampMs < lastProcessedTimestamp.current) {
      return;
    }

    lastProcessedTimestamp.current = Math.max(lastProcessedTimestamp.current, event.timestampMs ?? 0);

    if (isNoiseEvent(event)) {
      setNoiseWarning({
        visible: true,
        baselineDb: event.baselineDb,
        thresholdDb: event.thresholdDb,
        levelDb: event.levelDb,
        persistenceMs: event.persistenceMs,
        triggeredAt: event.timestampMs,
      });
      return;
    }

    if (isCountdownEvent(event)) {
      setCountdown((previous) => {
        const base = {
          totalMs: event.totalMs,
          remainingMs: Math.max(0, Math.min(event.remainingMs, event.totalMs)),
          cancelReason: event.cancelReason ?? null,
          updatedAt: event.timestampMs,
        };

        if (event.state === "canceled") {
          return { ...base, phase: "canceled" };
        }

        if (event.state === "completed") {
          return { ...base, remainingMs: 0, phase: "completed", cancelReason: null };
        }

        return { ...base, phase: event.state };
      });
      return;
    }

    if (isAutoStopEvent(event)) {
      setAutoStop({ reason: event.reason, timestampMs: event.timestampMs });
      setCountdown((previous) => {
        if (previous.phase === "canceled") {
          return previous;
        }
        return {
          ...previous,
          phase: "completed",
          remainingMs: 0,
          updatedAt: event.timestampMs,
          cancelReason: null,
        };
      });
    }
  }, []);

  useEffect(() => {
    let active = true;
    let unsubscribe: UnlistenFn | null = null;

    const bootstrap = async () => {
      try {
        const history = (await invoke(SESSION_EVENT_HISTORY_COMMAND)) as unknown;
        if (!active || !Array.isArray(history)) {
          return;
        }
        history
          .map(coerceEventPayload)
          .filter((event): event is SessionEventPayload => Boolean(event))
          .forEach((event) => applyEvent(event));
      } catch (error) {
        console.warn("failed to hydrate session events", error);
      }

      try {
        unsubscribe = await listen<SessionEventPayload>(SESSION_EVENT_CHANNEL, (event) => {
          const parsed = coerceEventPayload(event.payload);
          if (parsed) {
            applyEvent(parsed);
          }
        });
      } catch (error) {
        console.warn("failed to subscribe to session events", error);
      }
    };

    bootstrap();

    return () => {
      active = false;
      if (unsubscribe) {
        unsubscribe();
      }
    };
  }, [applyEvent]);

  useEffect(() => {
    if (!noiseWarning.visible) {
      return;
    }

    const handle = window.setTimeout(() => {
      setNoiseWarning((current) => ({ ...current, visible: false }));
    }, NOISE_DISMISS_DELAY_MS);

    return () => window.clearTimeout(handle);
  }, [noiseWarning.visible, noiseWarning.triggeredAt]);

  const state = useMemo<SessionEventsState>(
    () => ({
      noiseWarning,
      dismissNoiseWarning,
      countdown,
      autoStop,
      resetAutoStop,
    }),
    [noiseWarning, dismissNoiseWarning, countdown, autoStop, resetAutoStop],
  );

  return state;
};

