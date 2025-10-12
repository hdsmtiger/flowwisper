import type { AutoStopState, CountdownState } from "./hooks/useSessionEvents";

type SilenceCountdownProps = {
  countdown: CountdownState;
  autoStop: AutoStopState;
  onDismissAutoStop: () => void;
};

const formatSeconds = (milliseconds: number): string => `${(milliseconds / 1000).toFixed(1)}s`;

const resolveCancellationMessage = (reason: CountdownState["cancelReason"]): string => {
  if (reason === "manualStop") {
    return "Recording ended manually before the silence timer completed.";
  }
  return "Speech detected. Countdown canceled and recording continues.";
};

export const SilenceCountdown = ({ countdown, autoStop, onDismissAutoStop }: SilenceCountdownProps) => {
  if (autoStop.reason) {
    return (
      <div className="silence-countdown" role="status" aria-live="polite">
        <div className="silence-countdown__content">
          <span className="silence-countdown__title">Recording ended automatically</span>
          <span className="silence-countdown__meta">
            We didn't detect speech for {formatSeconds(countdown.totalMs)}. Resume when you're ready.
          </span>
        </div>
        <button type="button" className="silence-countdown__action" onClick={onDismissAutoStop}>
          Got it
        </button>
      </div>
    );
  }

  if (countdown.phase === "idle") {
    return null;
  }

  if (countdown.phase === "canceled") {
    return (
      <div className="silence-countdown" role="status" aria-live="polite">
        <div className="silence-countdown__content">
          <span className="silence-countdown__title">Silence countdown canceled</span>
          <span className="silence-countdown__meta">{resolveCancellationMessage(countdown.cancelReason)}</span>
        </div>
      </div>
    );
  }

  const remaining = formatSeconds(Math.max(0, countdown.remainingMs));
  const progress = countdown.totalMs
    ? Math.min(1, Math.max(0, (countdown.totalMs - countdown.remainingMs) / countdown.totalMs))
    : 0;
  const message =
    countdown.phase === "completed"
      ? "Silence threshold reached. Finishing session..."
      : `No speech detected. Auto-ending in ${remaining}.`;

  return (
    <div className="silence-countdown silence-countdown--active" role="status" aria-live="assertive">
      <div className="silence-countdown__content">
        <span className="silence-countdown__title">Silence detected</span>
        <span className="silence-countdown__meta">{message}</span>
      </div>
      <div className="silence-countdown__meter" aria-hidden="true">
        <div
          className="silence-countdown__meter-fill"
          style={{ width: `${(progress * 100).toFixed(1)}%` }}
        />
      </div>
    </div>
  );
};

