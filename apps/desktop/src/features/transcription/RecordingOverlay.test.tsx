import { fireEvent, render, screen } from "@testing-library/react";
import { describe, expect, it, vi } from "vitest";

import { NoiseBanner } from "./NoiseBanner";
import { SilenceCountdown } from "./SilenceCountdown";
import type {
  AutoStopState,
  CountdownState,
  NoiseWarningState,
} from "./hooks/useSessionEvents";

describe("Recording overlay components", () => {
  it("renders a visible noise banner with formatted levels", () => {
    const warning: NoiseWarningState = {
      visible: true,
      baselineDb: 32.4,
      thresholdDb: 45.0,
      levelDb: 62.5,
      persistenceMs: 350,
      triggeredAt: Date.now(),
    };
    const onDismiss = vi.fn();

    render(<NoiseBanner warning={warning} onDismiss={onDismiss} />);

    expect(screen.getByText(/high background noise detected/i)).toBeInTheDocument();
    expect(screen.getByText(/62.5 dB/i)).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /dismiss/i }));
    expect(onDismiss).toHaveBeenCalledTimes(1);
  });

  it("does not render the noise banner when hidden", () => {
    const warning: NoiseWarningState = {
      visible: false,
      baselineDb: 0,
      thresholdDb: 0,
      levelDb: 0,
      persistenceMs: 0,
      triggeredAt: 0,
    };

    const { container } = render(
      <NoiseBanner warning={warning} onDismiss={vi.fn()} />,
    );

    expect(container).toBeEmptyDOMElement();
  });

  it("shows cancellation messaging when the silence countdown is canceled", () => {
    const countdown: CountdownState = {
      phase: "canceled",
      totalMs: 5000,
      remainingMs: 2000,
      cancelReason: "speechDetected",
      updatedAt: Date.now(),
    };
    const autoStop: AutoStopState = { reason: null, timestampMs: null };

    render(
      <SilenceCountdown
        countdown={countdown}
        autoStop={autoStop}
        onDismissAutoStop={vi.fn()}
      />,
    );

    expect(
      screen.getByText(/silence countdown canceled/i),
    ).toBeInTheDocument();
    expect(screen.getByText(/speech detected/i)).toBeInTheDocument();
  });

  it("renders an active countdown with progress", () => {
    const countdown: CountdownState = {
      phase: "tick",
      totalMs: 5000,
      remainingMs: 2500,
      cancelReason: null,
      updatedAt: Date.now(),
    };
    const autoStop: AutoStopState = { reason: null, timestampMs: null };

    const { container } = render(
      <SilenceCountdown
        countdown={countdown}
        autoStop={autoStop}
        onDismissAutoStop={vi.fn()}
      />,
    );

    expect(screen.getByText(/silence detected/i)).toBeInTheDocument();
    expect(screen.getByText(/auto-ending in/i)).toBeInTheDocument();
    const meterFill = container.querySelector(
      ".silence-countdown__meter-fill",
    ) as HTMLElement;
    expect(parseFloat(meterFill.style.width)).toBeCloseTo(50, 1);
  });

  it("displays auto-stop messaging and allows acknowledgement", () => {
    const countdown: CountdownState = {
      phase: "completed",
      totalMs: 5000,
      remainingMs: 0,
      cancelReason: null,
      updatedAt: Date.now(),
    };
    const autoStop: AutoStopState = {
      reason: "silenceTimeout",
      timestampMs: Date.now(),
    };
    const onDismiss = vi.fn();

    render(
      <SilenceCountdown
        countdown={countdown}
        autoStop={autoStop}
        onDismissAutoStop={onDismiss}
      />,
    );

    expect(
      screen.getByText(/recording ended automatically/i),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByRole("button", { name: /got it/i }));
    expect(onDismiss).toHaveBeenCalledTimes(1);
  });
});

