import type { NoiseWarningState } from "./hooks/useSessionEvents";

type NoiseBannerProps = {
  warning: NoiseWarningState;
  onDismiss: () => void;
};

const formatDb = (value: number): string => `${value.toFixed(1)} dB`;
const formatSeconds = (milliseconds: number): string => `${(milliseconds / 1000).toFixed(1)}s`;

export const NoiseBanner = ({ warning, onDismiss }: NoiseBannerProps) => {
  if (!warning.visible) {
    return null;
  }

  const delta = warning.levelDb - warning.thresholdDb;
  const severity = delta >= 0 ? `+${delta.toFixed(1)} dB over threshold` : "below threshold";

  return (
    <div className="noise-banner" role="alert" aria-live="assertive">
      <div className="noise-banner__content">
        <span className="noise-banner__title">High background noise detected</span>
        <span className="noise-banner__meta">
          Input level {formatDb(warning.levelDb)} · Threshold {formatDb(warning.thresholdDb)} · Baseline{' '}
          {formatDb(warning.baselineDb)} ({severity})
        </span>
        <span className="noise-banner__meta">
          Spike persisted for {formatSeconds(warning.persistenceMs)}. Move to a quieter place or mute unused microphones.
        </span>
      </div>
      <button type="button" className="noise-banner__dismiss" onClick={onDismiss}>
        Dismiss
      </button>
    </div>
  );
};

