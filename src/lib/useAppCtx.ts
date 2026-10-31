import { useEffect, useRef, useState } from "react";
import { api } from "./api";
import type { AppCtx } from "./ctx";
import { useDiskFree, useNow, useSettings, useStatus } from "./hooks";
import type { ChannelSetting, Encoder, VideoDevice } from "../types";

const NEW_CHANNEL: ChannelSetting = { enabled: true, device: null, size: null, fps: 30, deinterlace: "off" };

/** State shared by the main window and every popped-out panel window (each has its own copy,
 *  kept in sync through the backend status and a settings broadcast). */
export function useAppState(autoAssign = false) {
  const [settings, update] = useSettings();
  const [status, refreshStatus] = useStatus();
  const [devices, setDevices] = useState<VideoDevice[]>([]);
  const [encoders, setEncoders] = useState<Encoder[]>([]);
  const [busy, setBusy] = useState(false);
  const [actionError, setActionError] = useState("");
  const [selected, setSelected] = useState<string | null>(null);
  // Starting a recording briefly has no status (the monitor stops, the recorder is not up yet).
  // Keep "live" through that gap so the preview keeps its last picture instead of blanking.
  const [stickyLive, setStickyLive] = useState(false);
  useEffect(() => {
    if (status != null) return setStickyLive(true);
    const t = window.setTimeout(() => setStickyLive(false), 15000);
    return () => window.clearTimeout(t);
  }, [status == null]);
  const recording = status?.recording ?? false;
  const now = useNow(recording);
  const free = useDiskFree(settings?.root);

  // Only replace the list when it really changed, so polling does not re-render everything.
  const [ready, setReady] = useState(false);
  const loadDevices = () =>
    api
      .listDevices()
      .then((d) => { setReady(true); return d; })
      .then((d) => setDevices((cur) => (cur.map((x) => x.name).join("|") === d.map((x) => x.name).join("|") ? cur : d)))
      .catch(() => setDevices([]));
  useEffect(() => {
    loadDevices();
    api.usableEncoders().then(setEncoders);
    // Notice a camera being plugged in without the user pressing refresh.
    const t = window.setInterval(() => {
      if (!document.hidden && !recording) loadDevices();
    }, 6000);
    return () => window.clearInterval(t);
  }, [recording]);

  // A camera that appears after startup goes to a channel that has no device yet (the test pattern),
  // so plugging in the capture card is enough. Only the main window does this.
  const seen = useRef<Set<string> | null>(null);
  useEffect(() => {
    if (!settings || !ready) return;
    const before = seen.current;
    seen.current = new Set(devices.map((d) => d.name));
    if (!autoAssign || recording || !before) return;
    const fresh = devices.find((d) => !before.has(d.name) && !settings.channels.some((c) => c.device === d.name));
    const slot = settings.channels.findIndex((c) => c.enabled && c.device === null);
    if (fresh && slot >= 0) {
      update((s) => ({ ...s, channels: s.channels.map((c, k) => (k === slot ? { ...c, device: fresh.name, size: null } : c)) }));
    }
  }, [devices, ready]);

  const toggleRecording = async () => {
    if (!settings) return;
    setBusy(true);
    setActionError("");
    try {
      if (recording) await api.stop();
      else await api.start(settings);
    } catch (e) {
      setActionError(String(e));
    }
    await refreshStatus();
    setBusy(false);
  };
  const runtimeErrors = (status?.channels ?? [])
    .filter((c) => c.last_error && c.state !== "recording")
    .map((c) => `CH${c.id}: ${c.last_error}`)
    .join("\n");
  const error = [actionError, ...(status?.warnings ?? []), runtimeErrors].filter(Boolean).join("\n");
  const elapsed = status ? (now - status.started_ms) / 1000 : 0;

  const ctx: AppCtx | null = settings && {
    settings,
    update,
    setChannel: (i, patch) =>
      update((s) => ({ ...s, channels: s.channels.map((c, k) => (k === i ? { ...c, ...patch } : c)) })),
    addChannel: () => update((s) => ({ ...s, channels: [...s.channels, { ...NEW_CHANNEL }] })),
    removeChannel: (i) => update((s) => ({ ...s, channels: s.channels.filter((_, k) => k !== i) })),
    statuses: status?.channels ?? [],
    recording,
    live: status != null || stickyLive,
    devices,
    encoders,
    refreshDevices: loadDevices,
    warnings: status?.warnings ?? [],
    layerActive: status?.layer_active ?? false,
    overlayEditable: !recording || (status?.layer_active ?? false),
    values: status?.values ?? {},
    selected,
    setSelected,
    toggleRecording,
    busy,
    elapsed,
    error,
    openFolder: () => settings && api.openFolder(settings.root).catch((e) => setActionError(String(e))),
  };
  return { ctx, settings, status, recording, elapsed, free, error };
}
