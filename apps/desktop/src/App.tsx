import { ReactNode, useEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

type SessionStatus = {
  phase: string;
  detail: string;
  timestamp_ms: number;
};

type PermissionResponse = {
  granted: boolean;
  manual_hint?: string | null;
  platform: string;
  detail?: string | null;
};

type PermissionStatus = {
  microphone: boolean;
  accessibility: boolean;
};

type PermissionResults = {
  microphone: PermissionResponse | null;
  accessibility: PermissionResponse | null;
};

type TutorialCompletionResponse = {
  finished: boolean;
  status?: string | null;
};

type AudioInputDevice = {
  id: string;
  label: string;
  kind: string;
  preferred: boolean;
};

type CalibrationMode = "auto" | "manual";

type CalibrationResult = {
  device_id: string;
  device_label: string;
  recommended_threshold: number;
  applied_threshold: number;
  noise_floor_db: number;
  sample_window_ms: number;
  frame_window_ms: number;
  mode: CalibrationMode;
  updated_at_ms?: number | null;
  noise_alert: boolean;
  noise_hint?: string | null;
  strong_noise_mode: boolean;
};

type AudioDiagnostics = {
  device_id: string;
  device_label: string;
  duration_ms: number;
  sample_rate: number;
  snr_db: number;
  peak_dbfs: number;
  rms_dbfs: number;
  noise_floor_db: number;
  noise_alert: boolean;
  noise_hint?: string | null;
  waveform: number[];
  sample_token: string;
  frame_window_ms: number;
};

type AudioMeterFrame = {
  context: string;
  device_id: string;
  peak: number;
  rms: number;
  vad_active: boolean;
  timestamp_ms: number;
};

type EnginePreferenceResponse = {
  choice: string | null;
  recommended: EngineChoice;
  privacy_notice: string;
};

type HotkeySource = "fn" | "custom";

type HotkeyBinding = {
  combination: string;
  source: HotkeySource;
  reason?: string | null;
};

type FnProbeResult = {
  supported: boolean;
  latency_ms?: number | null;
  raw_latency_ns?: number | null;
  user_reaction_ms?: number | null;
  within_sla?: boolean | null;
  interface?: string | null;
  device_origin?: string | null;
  reason?: string | null;
};

const FN_WAVE_BUCKETS = 18;

type HotkeyCaptureResponse = {
  combination: string;
  conflict_with?: string | null;
  reason?: string | null;
};

type OnboardingStep =
  | "welcome"
  | "permissions"
  | "device"
  | "calibration"
  | "engine"
  | "hotkey"
  | "tutorial";

type TutorialPhase =
  | "idle"
  | "priming"
  | "recording"
  | "playback"
  | "commands"
  | "complete";

type EngineChoice = "local" | "cloud" | "hybrid";

const ORDERED_STEPS: OnboardingStep[] = [
  "welcome",
  "permissions",
  "device",
  "calibration",
  "engine",
  "hotkey",
  "tutorial",
];

const isEngineChoice = (value: string | null | undefined): value is EngineChoice =>
  value === "local" || value === "cloud" || value === "hybrid";

export default function App() {
  const [status, setStatus] = useState<SessionStatus | null>(null);
  const [timeline, setTimeline] = useState<SessionStatus[]>([]);
  const [binding, setBinding] = useState<HotkeyBinding | null>(null);
  const [reservedConflicts, setReservedConflicts] = useState<string[]>([]);
  const [fnSupport, setFnSupport] = useState<"unknown" | "supported" | "unsupported">(
    "unknown"
  );
  const [fnLatency, setFnLatency] = useState<number | null>(null);
  const [fnReaction, setFnReaction] = useState<number | null>(null);
  const [fnWithinSla, setFnWithinSla] = useState<boolean | null>(null);
  const [probeReason, setProbeReason] = useState<string | null>(null);
  const [probingFn, setProbingFn] = useState(false);
  const [fnOverlay, setFnOverlay] = useState({
    phase: "hidden",
    detail: "",
  });
  const [fnWave, setFnWave] = useState<number[]>(
    Array.from({ length: FN_WAVE_BUCKETS }, () => 0)
  );
  const [fnVadActive, setFnVadActive] = useState(false);
  const [recordingCustom, setRecordingCustom] = useState(false);
  const [conflict, setConflict] = useState<string | null>(null);
  const [feedback, setFeedback] = useState<string | null>(null);
  const [hotkeyError, setHotkeyError] = useState<string | null>(null);
  const [saving, setSaving] = useState(false);
  const [fnPhase, setFnPhase] = useState<
    "idle" | "arming" | "waiting" | "degraded" | "detected" | "failed"
  >("idle");
  const [step, setStep] = useState<OnboardingStep>("welcome");
  const [permissionStatus, setPermissionStatus] = useState<PermissionStatus>({
    microphone: false,
    accessibility: false,
  });
  const [permissionResults, setPermissionResults] =
    useState<PermissionResults>({
      microphone: null,
      accessibility: null,
    });
  const [permissionLoading, setPermissionLoading] = useState({
    microphone: false,
    accessibility: false,
  });
  const [devices, setDevices] = useState<AudioInputDevice[]>([]);
  const [selectedDevice, setSelectedDevice] = useState<string | null>(null);
  const [persistedDevice, setPersistedDevice] = useState<string | null>(null);
  const [testingDevice, setTestingDevice] = useState(false);
  const [deviceDiagnostics, setDeviceDiagnostics] =
    useState<AudioDiagnostics | null>(null);
  const [deviceWaveform, setDeviceWaveform] = useState<number[]>(
    Array.from({ length: 180 }, () => 0)
  );
  const [liveDeviceWave, setLiveDeviceWave] = useState<number[]>(
    Array.from({ length: 90 }, () => 0)
  );
  const [sampleUrl, setSampleUrl] = useState<string | null>(null);
  const [strongNoiseMode, setStrongNoiseMode] = useState(false);
  const [strongNoiseAcknowledged, setStrongNoiseAcknowledged] =
    useState(false);
  const [calibration, setCalibration] = useState(0.62);
  const [calibrationResult, setCalibrationResult] = useState<
    CalibrationResult | null
  >(null);
  const [calibrationMode, setCalibrationMode] =
    useState<CalibrationMode>("auto");
  const [calibrationPersisted, setCalibrationPersisted] = useState(false);
  const [calibrationSaving, setCalibrationSaving] = useState(false);
  const [calibrationFeedback, setCalibrationFeedback] = useState<string | null>(
    null
  );
  const [calibrating, setCalibrating] = useState(false);
  const [engineChoice, setEngineChoice] = useState<EngineChoice | null>(null);
  const [enginePref, setEnginePref] = useState<EnginePreferenceResponse | null>(
    null
  );
  const [hotkeySaved, setHotkeySaved] = useState(false);
  const [globalError, setGlobalError] = useState<string | null>(null);
  const [tutorialRunning, setTutorialRunning] = useState(false);
  const [tutorialElapsed, setTutorialElapsed] = useState(0);
  const [tutorialOutcome, setTutorialOutcome] = useState<
    "none" | "completed" | "skipped"
  >("none");
  const [tutorialPhase, setTutorialPhase] =
    useState<TutorialPhase>("idle");
  const [tutorialChecklist, setTutorialChecklist] = useState({
    primed: false,
    playback: false,
    command: false,
  });
  const tutorialChecklistRef = useRef({
    primed: false,
    playback: false,
    command: false,
  });
  const [overlayWave, setOverlayWave] = useState<number[]>(
    Array.from({ length: 24 }, () => 0)
  );
  const [tutorialCommandPaused, setTutorialCommandPaused] =
    useState(false);

  const armingTimerRef = useRef<number | null>(null);
  const slaTimerRef = useRef<number | null>(null);
  const tutorialTimerRef = useRef<number | null>(null);
  const tutorialTimeoutRef = useRef<number | null>(null);
  const tutorialHalfRef = useRef<number | null>(null);
  const tutorialPhaseTimerRef = useRef<number | null>(null);
  const tutorialWaveTimerRef = useRef<number | null>(null);
  const tutorialAudioRef = useRef<AudioContext | null>(null);
  const fnPhaseRef = useRef<
    "idle" | "arming" | "waiting" | "degraded" | "detected" | "failed"
  >(fnPhase);
  const fnAudioRef = useRef<AudioContext | null>(null);
  const fnWaveRef = useRef(fnWave);

  const tutorialCompleted = tutorialOutcome === "completed";
  const tutorialSkipped = tutorialOutcome === "skipped";
  const testingDeviceRef = useRef(testingDevice);
  const selectedDeviceRef = useRef<string | null>(selectedDevice);
  const probingFnRef = useRef(probingFn);

  useEffect(() => {
    invoke<SessionStatus>("session_status")
      .then(setStatus)
      .catch(() =>
        setStatus({
          phase: "Unknown",
          detail: "Core service unreachable",
          timestamp_ms: Date.now(),
        })
      );
    invoke<SessionStatus[]>("session_timeline")
      .then(setTimeline)
      .catch(() => setTimeline([]));
    invoke<HotkeyBinding>("get_hotkey_binding")
      .then((result) => {
        setBinding(result);
        if (result.source === "fn" && result.combination.toLowerCase() === "fn") {
          setFnSupport("supported");
        } else if (result.source === "custom") {
          setFnSupport("unsupported");
        }
      })
      .catch(() => {
        setBinding({ combination: "", source: "custom", reason: null });
      });
    invoke<string[]>("list_hotkey_conflicts")
      .then(setReservedConflicts)
      .catch(() => setReservedConflicts([]));

    let sessionUnlisten: (() => void) | undefined;
    let meterUnlisten: (() => void) | undefined;

    listen<SessionStatus>("session://state", (event) => {
      setStatus(event.payload);
      setTimeline((prev) =>
        [...prev.slice(-19), event.payload].sort(
          (a, b) => a.timestamp_ms - b.timestamp_ms
        )
      );
      const { phase, detail } = event.payload;
      if (phase === "Priming" && detail.includes("Fn")) {
        setFnOverlay({ phase: "priming", detail });
      } else if (phase === "PreRoll" && detail.includes("Fn")) {
        setFnOverlay({ phase: "preroll", detail });
      } else if (phase === "Fallback" && detail.includes("Fn")) {
        setFnOverlay({ phase: "fallback", detail });
        setFnVadActive(false);
      } else if ((phase === "Ready" || phase === "Idle") && detail.includes("Fn")) {
        setFnOverlay({ phase: "hidden", detail: "" });
        setFnVadActive(false);
      }
    }).then((fn) => {
      sessionUnlisten = fn;
    });

    listen<AudioMeterFrame>("audio://meter", (event) => {
      const payload = event.payload;
      if (payload.context === "device-test") {
        const watchingDevice = selectedDeviceRef.current;
        if (
          testingDeviceRef.current &&
          (!watchingDevice || payload.device_id === watchingDevice)
        ) {
          setLiveDeviceWave((prev) => {
            const window = prev.length || 90;
            const next = prev.slice(-(window - 1));
            next.push(Math.min(1, Math.max(0, payload.peak)));
            return next;
          });
        }
        return;
      }

      if (payload.context === "fn-preroll") {
        if (!probingFnRef.current) {
          return;
        }
        setFnVadActive(Boolean(payload.vad_active));
        setFnWave((prev) => {
          const window = prev.length || FN_WAVE_BUCKETS;
          const next = prev.slice(-(window - 1));
          next.push(Math.min(1, Math.max(0, payload.peak)));
          return next;
        });
      }
    }).then((fn) => {
      meterUnlisten = fn;
    });

    return () => {
      sessionUnlisten?.();
      meterUnlisten?.();
      if (armingTimerRef.current != null) {
        window.clearTimeout(armingTimerRef.current);
      }
      if (slaTimerRef.current != null) {
        window.clearTimeout(slaTimerRef.current);
      }
      if (tutorialTimerRef.current != null) {
        window.clearInterval(tutorialTimerRef.current);
      }
      if (tutorialTimeoutRef.current != null) {
        window.clearTimeout(tutorialTimeoutRef.current);
      }
      if (tutorialHalfRef.current != null) {
        window.clearTimeout(tutorialHalfRef.current);
      }
      if (tutorialAudioRef.current) {
        tutorialAudioRef.current.close();
        tutorialAudioRef.current = null;
      }
      if (fnAudioRef.current) {
        fnAudioRef.current.close();
        fnAudioRef.current = null;
      }
    };
  }, []);

  useEffect(() => {
    invoke<string | null>("get_selected_microphone")
      .then((deviceId) => setPersistedDevice(deviceId))
      .catch(() => setPersistedDevice(null));
  }, []);

  useEffect(() => {
    fnPhaseRef.current = fnPhase;
  }, [fnPhase]);

  useEffect(() => {
    fnWaveRef.current = fnWave;
  }, [fnWave]);

  useEffect(() => {
    testingDeviceRef.current = testingDevice;
  }, [testingDevice]);

  useEffect(() => {
    selectedDeviceRef.current = selectedDevice;
  }, [selectedDevice]);

  useEffect(() => {
    probingFnRef.current = probingFn;
  }, [probingFn]);

  useEffect(() => {
    if (step === "device" && devices.length === 0) {
      invoke<AudioInputDevice[]>("list_audio_inputs")
        .then((items) => {
          setDevices(items);
        })
        .catch((err) =>
          setGlobalError(err instanceof Error ? err.message : String(err))
        );
    }
  }, [step, devices.length]);

  useEffect(() => {
    if (devices.length === 0) {
      return;
    }
    if (!selectedDevice) {
      if (persistedDevice && devices.some((d) => d.id === persistedDevice)) {
        setSelectedDevice(persistedDevice);
        return;
      }
      const preferred = devices.find((item) => item.preferred);
      setSelectedDevice((preferred ?? devices[0]).id);
    }
  }, [devices, persistedDevice, selectedDevice]);

  useEffect(() => {
    if (!enginePref) {
      invoke<EnginePreferenceResponse>("get_engine_preference")
        .then((pref) => {
          setEnginePref(pref);
          if (isEngineChoice(pref.choice)) {
            setEngineChoice(pref.choice);
          } else if (isEngineChoice(pref.recommended)) {
            setEngineChoice(pref.recommended);
          }
        })
        .catch(() => setEnginePref(null));
    }
  }, [enginePref]);

  useEffect(() => {
    invoke<TutorialCompletionResponse>("tutorial_completion")
      .then((result) => {
        const normalized = (result.status ?? undefined)?.toLowerCase();
        if (normalized === "skipped") {
          setTutorialOutcome("skipped");
          setTutorialElapsed(0);
        } else if (normalized === "completed" || result.finished) {
          setTutorialOutcome("completed");
          setTutorialElapsed(30);
        } else {
          setTutorialOutcome("none");
          setTutorialElapsed(0);
        }
      })
      .catch(() => setTutorialOutcome("none"));
  }, []);

  useEffect(() => {
    if (status?.phase === "Completed") {
      setTutorialOutcome("completed");
    }
    if (status?.phase === "TutorialSkipped") {
      setTutorialOutcome("skipped");
    }
  }, [status]);

  useEffect(() => {
    tutorialChecklistRef.current = tutorialChecklist;
  }, [tutorialChecklist]);

  useEffect(() => {
    setDeviceDiagnostics(null);
    setDeviceWaveform(Array.from({ length: 180 }, () => 0));
    setLiveDeviceWave((prev) => prev.map(() => 0));
    setSampleUrl(null);
    setStrongNoiseMode(false);
    setStrongNoiseAcknowledged(false);
    setCalibrationResult(null);
    setCalibration(0.62);
    setCalibrationMode("auto");
    setCalibrationPersisted(false);
    setCalibrationFeedback(null);
    setCalibrationSaving(false);
  }, [selectedDevice]);

  useEffect(() => {
    if (step === "calibration" && selectedDevice) {
      invoke<CalibrationResult | null>("get_device_calibration", {
        deviceId: selectedDevice,
      })
        .then((result) => {
          if (result) {
            const effectiveStrong = result.noise_alert
              ? result.strong_noise_mode
              : false;
            const normalizedResult = {
              ...result,
              strong_noise_mode: effectiveStrong,
            };
            setCalibrationResult(normalizedResult);
            setCalibration(normalizedResult.applied_threshold);
            setCalibrationMode(normalizedResult.mode);
            setStrongNoiseMode(effectiveStrong);
            setStrongNoiseAcknowledged(effectiveStrong);
            setCalibrationPersisted(
              !normalizedResult.noise_alert || effectiveStrong
            );
            setCalibrationFeedback(
              `已加载上次保存的${
                result.mode === "manual" ? "手动" : "自动"
              }阈值`
            );
          }
        })
        .catch(() => undefined);
    }
  }, [step, selectedDevice]);

  const clearProbeTimers = () => {
    if (armingTimerRef.current != null) {
      window.clearTimeout(armingTimerRef.current);
      armingTimerRef.current = null;
    }
    if (slaTimerRef.current != null) {
      window.clearTimeout(slaTimerRef.current);
      slaTimerRef.current = null;
    }
  };

  const uniformFnWave = (value: number) => {
    const bucketCount =
      fnWaveRef.current.length > 0 ? fnWaveRef.current.length : FN_WAVE_BUCKETS;
    const normalized = Math.min(1, Math.max(0, value));
    return Array.from({ length: bucketCount }, () => normalized);
  };

  const resetFnWave = () => {
    setFnWave(uniformFnWave(0));
    setFnVadActive(false);
  };

  const seedFnWave = (value: number) => {
    setFnWave(uniformFnWave(value));
  };

  const finalizeFnWave = (latency: number | null, withinSla: boolean | null) => {
    if (latency == null) {
      seedFnWave(0.2);
      return;
    }
    const normalized = Math.min(1, Math.max(0.25, 1 - latency / 500));
    const adjusted = withinSla === false ? Math.max(0.35, normalized * 0.65) : normalized;
    seedFnWave(adjusted);
  };

  const playFnTone = (phase: "start" | "success" | "failure") => {
    try {
      if (!fnAudioRef.current) {
        fnAudioRef.current = new AudioContext();
      }
      const ctx = fnAudioRef.current;
      if (!ctx) return;
      if (ctx.state === "suspended") {
        ctx.resume().catch(() => undefined);
      }
      const oscillator = ctx.createOscillator();
      const gain = ctx.createGain();
      oscillator.type = "sine";
      oscillator.frequency.value =
        phase === "success" ? 880 : phase === "failure" ? 240 : 520;
      const now = ctx.currentTime;
      gain.gain.setValueAtTime(0.0001, now);
      const peakGain = phase === "success" ? 0.3 : phase === "failure" ? 0.22 : 0.18;
      const attack = phase === "start" ? 0.015 : 0.02;
      const release = phase === "start" ? 0.22 : 0.32;
      gain.gain.exponentialRampToValueAtTime(peakGain, now + attack);
      gain.gain.exponentialRampToValueAtTime(0.0001, now + release);
      oscillator.connect(gain);
      gain.connect(ctx.destination);
      oscillator.start(now);
      oscillator.stop(now + release + 0.02);
    } catch {
      // ignore audio errors
    }
  };

  const startFnDetection = () => {
    if (probingFn) return;
    clearProbeTimers();
    setFeedback(null);
    setHotkeyError(null);
    setProbeReason(null);
    setFnLatency(null);
    setFnReaction(null);
    setFnWithinSla(null);
    setConflict(null);
    setFnSupport("unknown");
    setFnPhase("arming");
    setHotkeySaved(false);
    setProbingFn(true);
    setFnOverlay({ phase: "priming", detail: "等待 Fn 驱动回调..." });
    resetFnWave();
    seedFnWave(0.1);
    playFnTone("start");
    armingTimerRef.current = window.setTimeout(() => {
      setFnPhase((phase) => (phase === "arming" ? "waiting" : phase));
    }, 90);
    slaTimerRef.current = window.setTimeout(() => {
      setFnPhase((phase) => {
        if (phase === "detected") {
          return phase;
        }
        return "degraded";
      });
    }, 400);
    invoke<FnProbeResult>("start_fn_probe")
      .then((result) => {
        clearProbeTimers();
        const latency = result.latency_ms ?? null;
        const origin = result.device_origin ?? null;
        const interfaceLabel = result.interface ?? null;
        const reaction = result.user_reaction_ms ?? null;
        const withinSla = result.within_sla ?? null;
        setFnLatency(latency);
        setFnReaction(reaction);
        setFnWithinSla(withinSla);
        finalizeFnWave(latency, withinSla);
        if (result.supported) {
          setFnSupport("supported");
          setProbeReason(result.reason ?? null);
          setBinding({
            combination: "Fn",
            source: "fn",
            reason: result.reason ?? null,
          });
          setConflict(null);
          setFnPhase((prev) => (prev === "failed" ? prev : "detected"));
          playFnTone("success");
          if (latency != null || origin || interfaceLabel || reaction != null) {
            const parts: string[] = [];
            if (origin) parts.push(origin);
            if (interfaceLabel) parts.push(interfaceLabel);
            if (reaction != null) {
              parts.push(`回调 ${Math.round(reaction)}ms`);
            }
            const latencyPart = latency != null ? `${Math.round(latency)}ms` : undefined;
            if (latencyPart) parts.push(`处理 ${latencyPart}`);
            const summary = parts.join(" · ");
            if (withinSla === false) {
              const reactionLabel = reaction != null ? `${Math.round(reaction)}ms` : "未知";
              setFeedback(
                `Fn 捕获成功，但驱动回调耗时 ${reactionLabel}（>400ms SLA）` +
                  (summary ? ` · ${summary}` : "")
              );
            } else {
              setFeedback(`已捕获 Fn${summary ? `（${summary}）` : ""}`);
            }
          } else {
            setFeedback("已捕获 Fn 键");
          }
        } else {
          setFnSupport("unsupported");
          let reason = result.reason ?? "未能在 400ms 内捕获 Fn 键。";
          if (origin) {
            reason += `（设备：${origin}）`;
          }
          if (interfaceLabel) {
            reason += ` 接口：${interfaceLabel}`;
          }
          setProbeReason(reason);
          setBinding((prev) => ({
            combination: prev?.source === "custom" ? prev.combination : "",
            source: "custom",
            reason,
          }));
          setFnOverlay({ phase: "fallback", detail: reason });
          setFnPhase((phase) => (phase === "detected" ? phase : "failed"));
          playFnTone("failure");
        }
      })
      .catch((err) => {
        clearProbeTimers();
        const message = err instanceof Error ? err.message : String(err);
        setHotkeyError(message);
        setFnSupport("unsupported");
        setFnOverlay({ phase: "fallback", detail: message });
        setFnPhase("failed");
        finalizeFnWave(null, null);
        playFnTone("failure");
      })
      .finally(() => {
        setProbingFn(false);
        setFnVadActive(false);
      });
  };

  const startCustomRecording = () => {
    if (recordingCustom) return;
    setFeedback(null);
    setHotkeyError(null);
    setConflict(null);
    setHotkeySaved(false);
    setRecordingCustom(true);
    invoke<HotkeyCaptureResponse>("capture_custom_hotkey")
      .then((result) => {
        const conflictWith = result.conflict_with ?? null;
        setConflict(
          conflictWith
            ? `组合 ${result.combination} 与系统快捷键 ${conflictWith} 冲突，请重新选择。`
            : null
        );
        const reason = result.reason ?? probeReason ?? "Fn 键未被系统捕获，已切换到备用组合。";
        setProbeReason(reason);
        setBinding({
          combination: result.combination,
          source: "custom",
          reason,
        });
        setFnSupport("unsupported");
      })
      .catch((err) => {
        setHotkeyError(err instanceof Error ? err.message : String(err));
      })
      .finally(() => {
        setRecordingCustom(false);
      });
  };

  const fnStatusLabel = useMemo(() => {
    switch (fnPhase) {
      case "arming":
        return "正在初始化兼容层监听...";
      case "waiting":
        return "监听已就绪，请按下 Fn 键（目标 400ms 内收到回调）";
      case "degraded":
        return "未在 400ms 内收到驱动回调，尝试备用路径...";
      case "detected": {
        if (fnWithinSla === false) {
          const latency = fnLatency != null ? Math.round(fnLatency) : "?";
          const reaction = fnReaction != null ? ` · 用户反应 ${Math.round(fnReaction)}ms` : "";
          return `已捕获 Fn 键，但驱动回调 ${latency}ms 超出 400ms SLA，可继续使用或录制备用组合${reaction}`;
        }
        if (fnLatency == null) {
          return "已捕获 Fn 键";
        }
        const latency = Math.round(fnLatency);
        return `已捕获 Fn 键（驱动回调 ${latency}ms）`;
      }
      case "failed":
        return probeReason ?? "Fn 回调超时或被系统保留，请录制备用组合。";
      default:
        return probingFn ? "正在通过兼容层检测 Fn..." : "尚未检测";
    }
  }, [fnLatency, fnPhase, probeReason, probingFn, fnWithinSla, fnReaction]);

  const handleSave = async () => {
    if (!binding?.combination || conflict) return;
    setSaving(true);
    setFeedback(null);
    setHotkeyError(null);
    try {
      const persisted = await invoke<HotkeyBinding>("persist_hotkey_binding", {
        request: {
          combination: binding.combination,
          source: binding.source,
          reason: binding.reason ?? null,
        },
      });
      setBinding(persisted);
      setFeedback("热键已保存，并同步至托盘。");
      setHotkeySaved(true);
      const freshStatus = await invoke<SessionStatus>("session_status");
      setStatus(freshStatus);
    } catch (err) {
      setHotkeyError(err instanceof Error ? err.message : String(err));
    } finally {
      setSaving(false);
    }
  };

  const refreshPermissionStatus = () => {
    invoke<PermissionStatus>("permission_status")
      .then((status) => setPermissionStatus(status))
      .catch(() =>
        setPermissionStatus({ microphone: false, accessibility: false })
      );
  };

  const beginWizard = () => {
    setGlobalError(null);
    refreshPermissionStatus();
    invoke<PermissionResponse>("check_accessibility_permission")
      .then((result) => {
        setPermissionResults((prev) => ({
          ...prev,
          accessibility: result,
        }));
        setPermissionStatus((prev) => ({
          ...prev,
          accessibility: result.granted,
        }));
      })
      .catch(() => undefined);
    invoke<SessionStatus>("prime_session_preroll")
      .then(setStatus)
      .catch((err) =>
        setGlobalError(err instanceof Error ? err.message : String(err))
      );
    setStep("permissions");
  };

  const requestSystemPermission = (kind: "microphone" | "accessibility") => {
    setPermissionLoading((prev) => ({ ...prev, [kind]: true }));
    setGlobalError(null);
    const command =
      kind === "microphone"
        ? "request_microphone_permission"
        : "request_accessibility_permission";
    invoke<PermissionResponse>(command)
      .then((result) => {
        setPermissionResults((prev) => ({ ...prev, [kind]: result }));
        setPermissionStatus((prev) => ({ ...prev, [kind]: result.granted }));
      })
      .catch((err) =>
        setGlobalError(err instanceof Error ? err.message : String(err))
      )
      .finally(() =>
        setPermissionLoading((prev) => ({ ...prev, [kind]: false }))
      );
  };

  const openPermissionSettings = (kind: "microphone" | "accessibility") => {
    const command =
      kind === "microphone"
        ? "open_microphone_privacy_settings"
        : "open_accessibility_privacy_settings";
    invoke<void>(command).catch((err) =>
      setGlobalError(err instanceof Error ? err.message : String(err))
    );
  };

  const startDeviceTest = () => {
    if (testingDevice) return;
    setTestingDevice(true);
    setGlobalError(null);
    setDeviceDiagnostics(null);
    setDeviceWaveform(Array.from({ length: 180 }, () => 0));
    setLiveDeviceWave((prev) => prev.map(() => 0));
    setSampleUrl(null);
    invoke<AudioDiagnostics>("run_audio_diagnostics", {
      deviceId: selectedDevice,
    })
      .then((report) => {
        setStrongNoiseAcknowledged((prev) =>
          report.noise_alert ? prev : false
        );
        if (!report.noise_alert) {
          setStrongNoiseMode(false);
        }
        setDeviceDiagnostics(report);
        setDeviceWaveform(report.waveform);
        invoke<string>("load_diagnostic_sample", {
          token: report.sample_token,
        })
          .then((encoded) =>
            setSampleUrl(`data:audio/wav;base64,${encoded}`)
          )
          .catch((err) =>
            setGlobalError(
              err instanceof Error ? err.message : String(err)
            )
          );
        setPersistedDevice(report.device_id);
        setSelectedDevice(report.device_id);
        invoke<void>("persist_selected_microphone", {
          deviceId: report.device_id,
        }).catch((err) =>
          setGlobalError(err instanceof Error ? err.message : String(err))
        );
      })
      .catch((err) =>
        setGlobalError(err instanceof Error ? err.message : String(err))
      )
      .finally(() => setTestingDevice(false));
  };

  const handleEnableStrongNoise = () => {
    setStrongNoiseAcknowledged(true);
    setStrongNoiseMode(true);
    setCalibrationResult((previous) =>
      previous ? { ...previous, strong_noise_mode: true } : previous
    );
    if (calibrationResult?.noise_alert) {
      setCalibrationPersisted(true);
    }
  };

  const handleDisableStrongNoise = () => {
    setStrongNoiseAcknowledged(false);
    setStrongNoiseMode(false);
    setCalibrationResult((previous) =>
      previous ? { ...previous, strong_noise_mode: false } : previous
    );
    if (calibrationResult?.noise_alert) {
      setCalibrationPersisted(false);
    }
  };

  const startWaveAnimation = (intensity: number) => {
    if (tutorialWaveTimerRef.current != null) {
      window.clearInterval(tutorialWaveTimerRef.current);
    }
    tutorialWaveTimerRef.current = window.setInterval(() => {
      setOverlayWave((previous) =>
        previous.map(() => Math.min(1, Math.random() * intensity))
      );
    }, 120);
  };

  const stopWaveAnimation = () => {
    if (tutorialWaveTimerRef.current != null) {
      window.clearInterval(tutorialWaveTimerRef.current);
      tutorialWaveTimerRef.current = null;
    }
    setOverlayWave(Array.from({ length: 24 }, () => 0));
  };

  const clearPhaseTimer = () => {
    if (tutorialPhaseTimerRef.current != null) {
      window.clearTimeout(tutorialPhaseTimerRef.current);
      tutorialPhaseTimerRef.current = null;
    }
  };

  const emitTutorialEvent = (phaseKey: string, detail: string) => {
    invoke<SessionStatus>("record_tutorial_event", {
      phase: phaseKey,
      detail,
    }).catch(() => undefined);
  };

  const enterTutorialPhase = (phase: TutorialPhase, detail: string) => {
    setTutorialPhase(phase);
    switch (phase) {
      case "priming":
        stopWaveAnimation();
        emitTutorialEvent("TutorialPriming", detail);
        clearPhaseTimer();
        break;
      case "recording":
        startWaveAnimation(1);
        emitTutorialEvent("TutorialRecording", detail);
        clearPhaseTimer();
        tutorialPhaseTimerRef.current = window.setTimeout(() => {
          enterTutorialPhase("playback", "自动回放演练");
        }, 12000);
        break;
      case "playback":
        startWaveAnimation(0.45);
        emitTutorialEvent("TutorialPlayback", detail);
        clearPhaseTimer();
        tutorialPhaseTimerRef.current = window.setTimeout(() => {
          enterTutorialPhase("commands", "练习语音指令与热键");
        }, 8000);
        break;
      case "commands":
        stopWaveAnimation();
        emitTutorialEvent("TutorialCommands", detail);
        clearPhaseTimer();
        break;
      case "complete":
        stopWaveAnimation();
        emitTutorialEvent("TutorialComplete", detail);
        clearPhaseTimer();
        break;
      default:
        break;
    }
  };

  const handleTutorialHotkey = () => {
    if (tutorialPhase !== "priming") {
      return;
    }
    setTutorialChecklist((prev) => ({ ...prev, primed: true }));
    enterTutorialPhase("recording", "模拟语音录制");
  };

  const handleTutorialPlayback = () => {
    if (tutorialPhase !== "playback") {
      return;
    }
    playTutorialBeep();
    setTutorialChecklist((prev) => ({ ...prev, playback: true }));
  };

  const handleTutorialCommandToggle = () => {
    if (tutorialPhase !== "commands") {
      return;
    }
    setTutorialCommandPaused((value) => !value);
    setTutorialChecklist((prev) => ({ ...prev, command: true }));
  };

  const runCalibration = () => {
    if (calibrating) return;
    setCalibrating(true);
    setGlobalError(null);
    setCalibrationFeedback(null);
    invoke<CalibrationResult>("calibrate_noise_floor", {
      deviceId: selectedDevice,
    })
      .then((result) => {
        const effectiveStrong = result.noise_alert
          ? result.strong_noise_mode || strongNoiseAcknowledged
          : false;
        const normalizedResult = {
          ...result,
          strong_noise_mode: effectiveStrong,
        };
        setCalibrationResult(normalizedResult);
        setCalibration(normalizedResult.applied_threshold);
        setCalibrationMode(normalizedResult.mode);
        setStrongNoiseMode(effectiveStrong);
        setStrongNoiseAcknowledged(effectiveStrong);
        setCalibrationPersisted(
          !normalizedResult.noise_alert || effectiveStrong
        );
        setCalibrationFeedback(
          `已写入自动模式阈值 ${normalizedResult.applied_threshold.toFixed(2)}`
        );
      })
      .catch((err) =>
        setGlobalError(err instanceof Error ? err.message : String(err))
      )
      .finally(() => setCalibrating(false));
  };

  const persistCalibrationPreference = (mode: CalibrationMode, value: number) => {
    const deviceId = selectedDevice ?? calibrationResult?.device_id ?? null;
    if (!deviceId) {
      setGlobalError("请选择需要校准的麦克风设备");
      return;
    }
    const base = calibrationResult;
    if (!base) {
      setGlobalError("请先运行自动校准以生成基线数据");
      return;
    }
    const recommended =
      base.recommended_threshold ?? (mode === "auto" ? value : base.applied_threshold);
    const deviceLabel =
      base.device_label ||
      devices.find((item) => item.id === deviceId)?.label ||
      "已保存设备";
    setCalibrationSaving(true);
    setGlobalError(null);
    setCalibrationFeedback(null);
    invoke<CalibrationResult>("persist_calibration_preference", {
        request: {
          deviceId,
          deviceLabel,
          threshold: value,
          recommendedThreshold: recommended,
          noiseFloorDb: base.noise_floor_db,
          sampleWindowMs: base.sample_window_ms,
          frameWindowMs: base.frame_window_ms,
          mode,
          noiseAlert: base.noise_alert,
          noiseHint: base.noise_hint ?? null,
          strongNoiseMode,
        },
    })
      .then((result) => {
        const effectiveStrong = result.noise_alert
          ? result.strong_noise_mode
          : false;
        const normalizedResult = {
          ...result,
          strong_noise_mode: effectiveStrong,
        };
        setCalibrationResult(normalizedResult);
        setCalibration(normalizedResult.applied_threshold);
        setCalibrationMode(normalizedResult.mode);
        setStrongNoiseMode(effectiveStrong);
        setStrongNoiseAcknowledged(effectiveStrong);
        setCalibrationPersisted(
          !normalizedResult.noise_alert || effectiveStrong
        );
        setCalibrationFeedback(
          `${normalizedResult.mode === "manual" ? "手动" : "自动"}阈值保存成功 (${normalizedResult.applied_threshold.toFixed(
            2
          )})`
        );
      })
      .catch((err) =>
        setGlobalError(err instanceof Error ? err.message : String(err))
      )
      .finally(() => setCalibrationSaving(false));
  };

  const handleCalibrationModeChange = (mode: CalibrationMode) => {
    if (calibrationMode === mode) return;
    if (!calibrationResult) {
      setCalibrationMode(mode);
      return;
    }
    if (mode === "auto") {
      setCalibrationMode("auto");
      persistCalibrationPreference("auto", calibrationResult.recommended_threshold);
    } else {
      setCalibrationMode("manual");
      setCalibration(calibrationResult.applied_threshold);
      setCalibrationFeedback("请调整滑杆并保存手动阈值");
    }
  };

  const stopTutorialTimers = () => {
    if (tutorialTimerRef.current != null) {
      window.clearInterval(tutorialTimerRef.current);
      tutorialTimerRef.current = null;
    }
    if (tutorialTimeoutRef.current != null) {
      window.clearTimeout(tutorialTimeoutRef.current);
      tutorialTimeoutRef.current = null;
    }
    if (tutorialHalfRef.current != null) {
      window.clearTimeout(tutorialHalfRef.current);
      tutorialHalfRef.current = null;
    }
    if (tutorialPhaseTimerRef.current != null) {
      window.clearTimeout(tutorialPhaseTimerRef.current);
      tutorialPhaseTimerRef.current = null;
    }
    if (tutorialWaveTimerRef.current != null) {
      window.clearInterval(tutorialWaveTimerRef.current);
      tutorialWaveTimerRef.current = null;
    }
    setOverlayWave(Array.from({ length: 24 }, () => 0));
  };

  const playTutorialBeep = () => {
    if (!tutorialAudioRef.current) {
      try {
        tutorialAudioRef.current = new AudioContext();
      } catch (err) {
        return;
      }
    }
    const context = tutorialAudioRef.current;
    if (!context) return;
    const oscillator = context.createOscillator();
    oscillator.type = "sine";
    oscillator.frequency.value = 880;
    const gain = context.createGain();
    gain.gain.setValueAtTime(0.0001, context.currentTime);
    gain.gain.exponentialRampToValueAtTime(
      0.35,
      context.currentTime + 0.05
    );
    gain.gain.exponentialRampToValueAtTime(
      0.0001,
      context.currentTime + 0.4
    );
    oscillator.connect(gain).connect(context.destination);
    oscillator.start();
    oscillator.stop(context.currentTime + 0.5);
  };

  const finalizeTutorial = () => {
    if (tutorialTimeoutRef.current != null) {
      window.clearTimeout(tutorialTimeoutRef.current);
      tutorialTimeoutRef.current = null;
    }
    if (
      !tutorialChecklistRef.current.primed ||
      !tutorialChecklistRef.current.playback ||
      !tutorialChecklistRef.current.command
    ) {
      setGlobalError("请完成浮层、回放与指令练习后再结束演练。");
      tutorialTimeoutRef.current = window.setTimeout(() => {
        finalizeTutorial();
      }, 5000);
      return;
    }
    stopTutorialTimers();
    setTutorialRunning(false);
    setTutorialElapsed(30);
    enterTutorialPhase("complete", "教程完成");
    window.setTimeout(() => setTutorialPhase("idle"), 800);
    invoke<SessionStatus>("complete_session_bootstrap")
      .then((snapshot) => {
        setStatus(snapshot);
        setTutorialOutcome("completed");
      })
      .catch((err) =>
        setGlobalError(err instanceof Error ? err.message : String(err))
      );
  };

  const skipTutorialStep = () => {
    stopTutorialTimers();
    setTutorialRunning(false);
    setTutorialPhase("idle");
    setTutorialElapsed(0);
    setTutorialOutcome("skipped");
    setTutorialChecklist({ primed: false, playback: false, command: false });
    tutorialChecklistRef.current = {
      primed: false,
      playback: false,
      command: false,
    };
    emitTutorialEvent("TutorialSkipped", "用户选择稍后查看教程");
    invoke<SessionStatus>("skip_tutorial")
      .then(setStatus)
      .catch((err) =>
        setGlobalError(err instanceof Error ? err.message : String(err))
      );
  };

  const startTutorial = async () => {
    if (tutorialRunning) {
      return;
    }
    stopTutorialTimers();
    setTutorialRunning(true);
    setTutorialElapsed(0);
    setTutorialOutcome("none");
    setGlobalError(null);
    setTutorialChecklist({ primed: false, playback: false, command: false });
    tutorialChecklistRef.current = {
      primed: false,
      playback: false,
      command: false,
    };
    setTutorialCommandPaused(false);
    try {
      if (!tutorialAudioRef.current) {
        tutorialAudioRef.current = new AudioContext();
      }
      await tutorialAudioRef.current?.resume();
    } catch (err) {
      // ignore audio context failures
    }
    setOverlayWave(Array.from({ length: 24 }, () => 0));
    enterTutorialPhase("priming", "浮层提示引导");
    playTutorialBeep();
    tutorialTimerRef.current = window.setInterval(() => {
      setTutorialElapsed((value) => Math.min(value + 1, 30));
    }, 1000);
    tutorialTimeoutRef.current = window.setTimeout(() => {
      finalizeTutorial();
    }, 30000);
  };

  const updateEngine = (choice: EngineChoice) => {
    setEngineChoice(choice);
    invoke<EnginePreferenceResponse>("persist_engine_preference", { choice })
      .then((pref) => setEnginePref(pref))
      .catch((err) =>
        setGlobalError(err instanceof Error ? err.message : String(err))
      );
  };

  const stepIndex = ORDERED_STEPS.indexOf(step);
  const goToStep = (target: OnboardingStep) => setStep(target);
  const goToNext = () => {
    const next = ORDERED_STEPS[Math.min(stepIndex + 1, ORDERED_STEPS.length - 1)];
    goToStep(next);
  };

  const tutorialReadyToComplete = useMemo(
    () =>
      tutorialChecklist.primed &&
      tutorialChecklist.playback &&
      tutorialChecklist.command,
    [tutorialChecklist]
  );

  const canProceed = useMemo(() => {
    switch (step) {
      case "welcome":
        return true;
      case "permissions":
        return permissionStatus.microphone && permissionStatus.accessibility;
      case "device":
        return (
          !!selectedDevice &&
          !!deviceDiagnostics &&
          !testingDevice &&
          (!deviceDiagnostics.noise_alert || strongNoiseAcknowledged)
        );
      case "calibration":
        return calibrationPersisted;
      case "engine":
        return engineChoice != null;
      case "hotkey":
        return hotkeySaved;
      case "tutorial":
        return tutorialOutcome !== "none";
      default:
        return false;
    }
  }, [
    step,
    permissionStatus,
    selectedDevice,
    testingDevice,
    deviceDiagnostics,
    calibrationPersisted,
    engineChoice,
    hotkeySaved,
    tutorialOutcome,
    strongNoiseAcknowledged,
  ]);

  useEffect(() => {
    if (step !== "hotkey" && fnOverlay.phase !== "hidden") {
      setFnOverlay({ phase: "hidden", detail: "" });
    }
  }, [step, fnOverlay.phase]);

  useEffect(() => {
    if (step === "permissions") {
      refreshPermissionStatus();
      invoke<PermissionResponse>("check_accessibility_permission")
        .then((result) => {
          setPermissionResults((prev) => ({
            ...prev,
            accessibility: result,
          }));
          setPermissionStatus((prev) => ({
            ...prev,
            accessibility: result.granted,
          }));
        })
        .catch(() => undefined);
    }
  }, [step]);

  const renderTutorialOverlay = () => {
    if (tutorialPhase === "idle") {
      return null;
    }
    if (tutorialPhase === "complete") {
      return (
        <div className="tutorial-overlay complete">
          <div className="overlay-card">
            <h3>演练完成</h3>
            <p>教程数据已同步，可继续完成向导。</p>
          </div>
        </div>
      );
    }

    let title = "";
    let description = "";
    let actions: ReactNode = null;
    const showWave = tutorialPhase === "recording" || tutorialPhase === "playback";

    switch (tutorialPhase) {
      case "priming":
        title = "按下 Fn 开始演练";
        description = "保持 Fn 按下或点击按钮以体验预热提示。";
        actions = (
          <button className="overlay-primary" onClick={handleTutorialHotkey}>
            模拟 Fn 触发
          </button>
        );
        break;
      case "recording":
        title = "录音中";
        description = "对着麦克风说话，观察实时波形反馈。";
        actions = <span className="overlay-hint">松开 Fn 将自动进入回放阶段。</span>;
        break;
      case "playback":
        title = "回放示例";
        description = "点击播放按钮体验 5 秒示例，并确认听写反馈。";
        actions = (
          <button className="overlay-primary" onClick={handleTutorialPlayback}>
            播放示例
          </button>
        );
        break;
      case "commands":
        title = "练习指令控制";
        description = "通过暂停/继续体验指令栏与快捷键联动。";
        actions = (
          <div className="overlay-command">
            <button className="overlay-primary" onClick={handleTutorialCommandToggle}>
              {tutorialCommandPaused ? "恢复录音" : "暂停录音"}
            </button>
            <span className="overlay-hint">
              当前状态：{tutorialCommandPaused ? "已暂停" : "录音进行中"}
            </span>
          </div>
        );
        break;
      default:
        break;
    }

    return (
      <div className={`tutorial-overlay phase-${tutorialPhase}`}>
        <div className="overlay-card">
          <header className="overlay-header">
            <span className="overlay-title">{title}</span>
            <span className="overlay-phase">{tutorialPhase.toUpperCase()}</span>
          </header>
          <div className="overlay-body">
            <p>{description}</p>
            {showWave && (
              <div className="overlay-wave">
                {overlayWave.map((value, index) => (
                  <span
                    key={`overlay-bar-${index}`}
                    style={{ height: `${Math.max(12, value * 100)}%` }}
                  />
                ))}
              </div>
            )}
            {actions}
          </div>
        </div>
      </div>
    );
  };

  const renderStep = () => {
    switch (step) {
      case "welcome":
        return (
          <div className="step-card">
            <h2>欢迎使用 Flowwisper</h2>
            <p>
              首次启动需要完成权限授权、设备测试、降噪校准、语音引擎选择以及 Fn
              兼容性检测，以满足 Sprint1 UC1.1-UC1.4 的验收要求。
            </p>
            <button className="primary" onClick={beginWizard}>
              开始设置
            </button>
          </div>
        );
      case "permissions":
        return (
          <div className="step-card">
            <h2>系统权限请求</h2>
            <p>
              需要同时授权麦克风与辅助功能权限，才能完成语音采集与 Fn 键捕获。
              如弹窗被拒绝，可跳转至系统设置手动启用。
            </p>
            <div className="permission-grid">
              <div className="permission-card">
                <h3>麦克风权限</h3>
                <p>授权后可运行设备诊断并进行降噪校准。</p>
                <div className="permission-actions">
                  <button
                    className="primary"
                    onClick={() => requestSystemPermission("microphone")}
                    disabled={permissionLoading.microphone}
                  >
                    {permissionLoading.microphone
                      ? "请求中..."
                      : permissionStatus.microphone
                      ? "重新检查"
                      : "请求麦克风权限"}
                  </button>
                  {!permissionStatus.microphone && (
                    <button
                      className="ghost"
                      onClick={() => openPermissionSettings("microphone")}
                    >
                      打开系统麦克风设置
                    </button>
                  )}
                </div>
                <div
                  className={`callout ${
                    permissionStatus.microphone ? "success" : "warning"
                  }`}
                >
                  {permissionStatus.microphone
                    ? "麦克风权限已授予，可继续进行设备检测。"
                    : permissionResults.microphone?.manual_hint ??
                      "请在系统设置中启用 Flowwisper 的麦克风访问。"}
                  {permissionResults.microphone?.detail && (
                    <div className="hint">{permissionResults.microphone.detail}</div>
                  )}
                </div>
              </div>
              <div className="permission-card">
                <h3>辅助功能权限</h3>
                <p>授权后可监听 Fn 驱动回调并校验 400ms SLA。</p>
                <div className="permission-actions">
                  <button
                    className="primary"
                    onClick={() => requestSystemPermission("accessibility")}
                    disabled={permissionLoading.accessibility}
                  >
                    {permissionLoading.accessibility
                      ? "请求中..."
                      : permissionStatus.accessibility
                      ? "重新检查"
                      : "请求辅助功能权限"}
                  </button>
                  {!permissionStatus.accessibility && (
                    <button
                      className="ghost"
                      onClick={() => openPermissionSettings("accessibility")}
                    >
                      打开辅助功能设置
                    </button>
                  )}
                </div>
                <div
                  className={`callout ${
                    permissionStatus.accessibility ? "success" : "warning"
                  }`}
                >
                  {permissionStatus.accessibility
                    ? "辅助功能权限已授予，Fn 捕获链路可用。"
                    : permissionResults.accessibility?.manual_hint ??
                      "请在隐私与安全 → 辅助功能中启用 Flowwisper。"}
                  {permissionResults.accessibility?.detail && (
                    <div className="hint">
                      {permissionResults.accessibility.detail}
                    </div>
                  )}
                </div>
              </div>
            </div>
          </div>
        );
      case "device":
        return (
          <div className="step-card">
            <h2>选择输入设备并测试</h2>
            <p>
              请选择常用麦克风，运行 5 秒诊断以验证拾音质量并生成可回放样本。
            </p>
            <div className="device-list">
              {devices.map((device) => (
                <label key={device.id} className="device-option">
                  <input
                    type="radio"
                    name="device"
                    value={device.id}
                    checked={selectedDevice === device.id}
                    onChange={() => setSelectedDevice(device.id)}
                  />
                  <span className="device-name">{device.label}</span>
                  <span className="device-kind">{device.kind}</span>
                </label>
              ))}
              {devices.length === 0 && <p>正在枚举可用麦克风...</p>}
            </div>
            <div className="device-actions">
              <button
                className="secondary"
                onClick={startDeviceTest}
                disabled={testingDevice}
              >
                {testingDevice ? "采集中..." : "运行 5 秒诊断"}
              </button>
              {testingDevice && (
                <span className="hint">正在录制样本，请对着麦克风读一段话。</span>
              )}
            </div>
            {deviceDiagnostics && (
              <div className="diagnostic-card">
                <h3>{deviceDiagnostics.device_label} 诊断结果</h3>
                <div className="metric-list">
                  <span>峰值 {deviceDiagnostics.peak_dbfs.toFixed(1)} dBFS</span>
                  <span>信噪比 {deviceDiagnostics.snr_db.toFixed(1)} dB</span>
                  <span>噪声 {deviceDiagnostics.noise_floor_db.toFixed(1)} dBFS</span>
                  <span>帧窗口 {deviceDiagnostics.frame_window_ms} ms</span>
                </div>
                <div className="waveform-bars">
                  {(testingDevice ? liveDeviceWave : deviceWaveform).map(
                    (value, index) => (
                    <span
                      key={`wave-${index}`}
                      style={{ height: `${Math.min(1, value) * 100}%` }}
                    />
                  ))}
                </div>
                {sampleUrl && (
                  <div className="sample-playback">
                    <span>样本回放：</span>
                    <audio controls src={sampleUrl} />
                  </div>
                )}
                {deviceDiagnostics.noise_alert ? (
                  <div className="callout warning">
                    <p>
                      {deviceDiagnostics.noise_hint ??
                        "检测到高噪声环境，请更换拾音位置或启用强降噪模式。"}
                    </p>
                    <div className="callout-actions">
                      <button
                        className="primary"
                        onClick={handleEnableStrongNoise}
                        disabled={strongNoiseAcknowledged}
                      >
                        {strongNoiseAcknowledged
                          ? "已启用强降噪模式"
                          : "启用强降噪模式"}
                      </button>
                      {strongNoiseAcknowledged && (
                        <button
                          className="ghost"
                          onClick={handleDisableStrongNoise}
                        >
                          取消强降噪
                        </button>
                      )}
                    </div>
                  </div>
                ) : (
                  <div className="callout success">
                    环境噪声处于可接受范围，可直接进入校准。
                  </div>
                )}
                {strongNoiseAcknowledged && (
                  <div className="callout info">
                    已记住强降噪偏好，后续校准与保存会同时写入该设置。
                  </div>
                )}
              </div>
            )}
          </div>
        );
      case "calibration":
        return (
          <div className="step-card">
            <h2>降噪与灵敏度校准</h2>
            <p>
              自动模式会采样 5 秒环境噪音并给出推荐阈值。可按需微调以平衡拾音灵敏度与背景噪音抑制。
            </p>
            <div className="calibration-mode">
              <span className="label">校准模式</span>
              <div className="mode-toggle">
                <button
                  type="button"
                  className={calibrationMode === "auto" ? "active" : ""}
                  onClick={() => handleCalibrationModeChange("auto")}
                  disabled={!calibrationResult || calibrationSaving || calibrating}
                >
                  自动推荐
                </button>
                <button
                  type="button"
                  className={calibrationMode === "manual" ? "active" : ""}
                  onClick={() => handleCalibrationModeChange("manual")}
                  disabled={!calibrationResult || calibrationSaving}
                >
                  手动调节
                </button>
              </div>
            </div>
            <div className="calibration-controls">
              <label>
                灵敏度阈值
                <input
                  type="range"
                  min={0}
                  max={1}
                  step={0.01}
                  value={calibration}
                  onChange={(event) => {
                    const value = Number(event.target.value);
                    setCalibration(value);
                    if (calibrationMode === "manual") {
                      setCalibrationPersisted(false);
                      setCalibrationFeedback(null);
                    }
                  }}
                  disabled={calibrationMode === "auto"}
                />
                <span className="calibration-value">{calibration.toFixed(2)}</span>
              </label>
            </div>
            <button className="secondary" onClick={runCalibration} disabled={calibrating}>
              {calibrating ? "采样中..." : "自动校准"}
            </button>
            <button
              className="primary"
              onClick={() => persistCalibrationPreference("manual", calibration)}
              disabled={
                calibrationMode !== "manual" ||
                calibrationSaving ||
                !calibrationResult
              }
            >
              {calibrationSaving ? "保存中..." : "保存当前阈值"}
            </button>
            {calibrationResult && (
              <div className="callout info">
                {`${calibrationResult.device_label} 环境噪声约 ${calibrationResult.noise_floor_db.toFixed(
                  1
                )} dB，推荐阈值 ${calibrationResult.recommended_threshold.toFixed(
                  2
                )}，当前应用 ${calibrationResult.applied_threshold.toFixed(2)}（采样窗口 ${
                  calibrationResult.sample_window_ms / 1000
                } 秒，帧窗口 ${calibrationResult.frame_window_ms} ms）。`}
              </div>
            )}
            {calibrationResult?.noise_alert && !strongNoiseMode && (
              <div className="callout warning">
                <p>
                  {calibrationResult.noise_hint ??
                    "环境噪声仍然较高，如需继续请启用强降噪模式或重新选择设备。"}
                </p>
                <div className="callout-actions">
                  <button
                    className="primary"
                    onClick={handleEnableStrongNoise}
                  >
                    启用强降噪模式
                  </button>
                </div>
              </div>
            )}
            {calibrationResult?.noise_alert && strongNoiseMode && (
              <div className="callout success">
                已启用强降噪模式，保存阈值后会同步写入噪音兜底配置。
              </div>
            )}
            {calibrationFeedback && (
              <div className="callout success">{calibrationFeedback}</div>
            )}
          </div>
        );
      case "engine":
        return (
          <div className="step-card">
            <h2>语音引擎模式</h2>
            <p>
              {enginePref?.privacy_notice ??
                "请选择默认转写模式，可随时在设置中调整。"}
            </p>
            <div className="engine-options">
              <label
                className={`engine-option ${
                  enginePref?.recommended === "local" ? "recommended" : ""
                }`}
              >
                <input
                  type="radio"
                  name="engine"
                  value="local"
                  checked={engineChoice === "local"}
                  onChange={() => updateEngine("local")}
                />
                <span className="engine-title">本地识别</span>
                <span className="engine-desc">16kHz 低延迟，适合离线或隐私场景。</span>
              </label>
              <label
                className={`engine-option ${
                  enginePref?.recommended === "cloud" ? "recommended" : ""
                }`}
              >
                <input
                  type="radio"
                  name="engine"
                  value="cloud"
                  checked={engineChoice === "cloud"}
                  onChange={() => updateEngine("cloud")}
                />
                <span className="engine-title">云端识别</span>
                <span className="engine-desc">高准确率，需网络连接与租户策略授权。</span>
              </label>
              <label
                className={`engine-option ${
                  enginePref?.recommended === "hybrid" ? "recommended" : ""
                }`}
              >
                <input
                  type="radio"
                  name="engine"
                  value="hybrid"
                  checked={engineChoice === "hybrid"}
                  onChange={() => updateEngine("hybrid")}
                />
                <span className="engine-title">智能混合</span>
                <span className="engine-desc">默认推荐，根据网络与设备性能自动切换。</span>
              </label>
            </div>
            {enginePref?.recommended && (
              <div className="hint">
                推荐模式：{enginePref.recommended === "hybrid" ? "智能混合" : enginePref.recommended === "cloud" ? "云端识别" : "本地识别"}
              </div>
            )}
          </div>
        );
      case "hotkey":
        return (
          <div className="step-card">
            <h2>Fn 热键绑定与兼容检测</h2>
            <p>
              在 400ms SLA 内验证 Fn 捕获能力，若不支持请录制备用组合并避免系统冲突。
            </p>
            {fnOverlay.phase !== "hidden" && (
              <div className={`fn-overlay ${fnOverlay.phase}`}>
                <div className="fn-wave">
                  {fnWave.map((value, index) => (
                    <span
                      key={`fn-wave-${index}`}
                      style={{ height: `${Math.max(8, value * 100)}%` }}
                    />
                  ))}
                </div>
                <div className="fn-overlay-text">
                  <strong>{fnOverlay.phase === "fallback" ? "Fn 捕获失败" : "Fn 预热"}</strong>
                  <span>{fnOverlay.detail}</span>
                  {fnOverlay.phase !== "fallback" && (
                    <span
                      className={`vad-indicator ${fnVadActive ? "active" : "idle"}`}
                    >
                      <span className="dot" />
                      {fnVadActive ? "拾音中" : "静音待命"}
                    </span>
                  )}
                  {fnOverlay.phase !== "fallback" && fnLatency != null && (
                    <span className={`sla ${fnWithinSla === false ? "warn" : "ok"}`}>
                      延迟 {Math.round(fnLatency)}ms
                      {fnWithinSla === false
                        ? "（超过 SLA）"
                        : fnWithinSla
                        ? "（满足 SLA）"
                        : ""}
                    </span>
                  )}
                </div>
              </div>
            )}
            <div className="hotkey-card">
              <div className="hotkey-status">
                <span className="label">捕获状态:</span>
                <span
                  className={`value ${fnSupport} phase-${fnPhase} ${
                    probingFn ? "highlight" : ""
                  }`}
                >
                  {fnStatusLabel}
                </span>
              </div>
              <button className="primary" onClick={startFnDetection} disabled={probingFn}>
                {probingFn ? "检测中..." : "测试 Fn 键"}
              </button>
              {probingFn && (
                <span className="hint">
                  兼容层将在 400ms 内给出首次反馈，请持续按下 Fn 键。
                </span>
              )}
              {probeReason && <div className="callout info">{probeReason}</div>}
            </div>

            <div className="hotkey-card">
              <div className="hotkey-status">
                <span className="label">当前组合:</span>
                <span className="value highlight">
                  {binding?.combination || "尚未绑定"}
                </span>
              </div>
              <button
                className="secondary"
                onClick={startCustomRecording}
                disabled={recordingCustom}
              >
                {recordingCustom ? "等待按键..." : "录制备用组合"}
              </button>
              {recordingCustom && (
                <span className="hint">请在 5 秒内按下希望绑定的组合键。</span>
              )}
              {reservedConflicts.length > 0 && (
                <span className="hint">
                  避免与系统快捷键冲突：{reservedConflicts.join("、")}。
                </span>
              )}
            </div>

            {binding?.reason && <div className="callout info">{binding.reason}</div>}
            {conflict && <div className="callout warning">{conflict}</div>}
            {feedback && <div className="callout success">{feedback}</div>}
            {hotkeyError && <div className="callout error">保存失败：{hotkeyError}</div>}

            <div className="actions">
              <button className="primary" onClick={handleSave} disabled={!binding?.combination || !!conflict || saving}>
                {saving ? "保存中..." : "保存热键"}
              </button>
            </div>
          </div>
        );
      case "tutorial":
        return (
          <div className="step-card">
            <h2>完成设置</h2>
            <p>
              已完成 Sprint1 onboarding。请通过浮层演练体验 Fn 预热、录音回放与指令控制，确保能独立完成操作。
            </p>
            <div className="tutorial-progress">
              <div className="progress-bar">
                <div
                  className="progress-value"
                  style={{ width: `${(tutorialElapsed / 30) * 100}%` }}
                />
              </div>
              <span>
                {tutorialElapsed.toString().padStart(2, "0")} / 30 秒
              </span>
            </div>
            <div className="tutorial-actions">
              <button
                className="primary"
                onClick={startTutorial}
                disabled={tutorialRunning}
              >
                {tutorialRunning
                  ? "演练进行中..."
                  : tutorialCompleted
                  ? "重新开始演练"
                  : "开始 30 秒演练"}
              </button>
              {tutorialRunning ? (
                <button
                  className="ghost"
                  onClick={finalizeTutorial}
                  disabled={!tutorialReadyToComplete}
                  title={
                    tutorialReadyToComplete
                      ? undefined
                      : "完成所有互动步骤后方可结束"
                  }
                >
                  提前结束并记录
                </button>
              ) : (
                <>
                  <button
                    className="ghost"
                    onClick={skipTutorialStep}
                    disabled={tutorialSkipped}
                  >
                    {tutorialSkipped ? "已标记稍后查看" : "稍后查看教程"}
                  </button>
                  {tutorialCompleted && (
                    <button
                      className="secondary"
                      onClick={finalizeTutorial}
                      disabled={!tutorialReadyToComplete}
                    >
                      重新标记完成
                    </button>
                  )}
                </>
              )}
            </div>
            {tutorialSkipped && (
              <div className="callout info">
                已记录“稍后查看”状态，可随时重新开始演练完成正式流程。
              </div>
            )}
            {tutorialCompleted && !tutorialRunning && (
              <div className="callout success">
                教程已完成，若需复习可重新开始演练或选择稍后查看。
              </div>
            )}
            <ul className="tutorial-checklist">
              <li className={tutorialChecklist.primed ? "done" : ""}>
                <input type="checkbox" checked={tutorialChecklist.primed} readOnly />
                <span>触发浮层并开始模拟录音</span>
              </li>
              <li className={tutorialChecklist.playback ? "done" : ""}>
                <input type="checkbox" checked={tutorialChecklist.playback} readOnly />
                <span>回放示例片段，确认听写反馈</span>
              </li>
              <li className={tutorialChecklist.command ? "done" : ""}>
                <input type="checkbox" checked={tutorialChecklist.command} readOnly />
                <span>切换暂停/继续，熟悉指令控制</span>
              </li>
            </ul>
          </div>
        );
      default:
        return null;
    }
  };

  return (
    <main className="app-shell">
      {renderTutorialOverlay()}
      <header className="hero">
        <h1>Flowwisper Fn</h1>
        <p>桌面端壳层脚手架 - Sprint1 首启向导</p>
        {status && (
          <div className="status-inline">
            <span className="phase">{status.phase}</span>
            <span className="detail">{status.detail}</span>
          </div>
        )}
      </header>

      <div className="content-grid">
        <aside className="timeline">
          <h2>会话状态日志</h2>
          <ul>
            {timeline.map((entry) => (
              <li key={`${entry.phase}-${entry.timestamp_ms}`}>
                <span className="phase">{entry.phase}</span>
                <span className="detail">{entry.detail}</span>
              </li>
            ))}
            {timeline.length === 0 && <li>暂无状态更新</li>}
          </ul>
        </aside>

        <section className="onboarding">
          <div className="step-indicator">
            {ORDERED_STEPS.map((item, index) => (
              <span
                key={item}
                className={`step-dot ${index <= stepIndex ? "active" : ""}`}
              />
            ))}
          </div>
          {globalError && <div className="callout error">{globalError}</div>}
          {renderStep()}
          <div className="wizard-nav">
            <button
              className="secondary"
              onClick={() =>
                goToStep(ORDERED_STEPS[Math.max(stepIndex - 1, 0)])
              }
              disabled={step === "welcome"}
            >
              上一步
            </button>
            <button
              className="primary"
              onClick={goToNext}
              disabled={!canProceed}
            >
              {step === "tutorial" ? "完成" : "下一步"}
            </button>
          </div>
        </section>
      </div>
    </main>
  );
}