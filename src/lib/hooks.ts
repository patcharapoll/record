import { useEffect, useRef, useState } from "react";
import { emit, listen } from "@tauri-apps/api/event";
import { getCurrentWindow } from "@tauri-apps/api/window";
import { api } from "./api";
import { selfTest, YuvRenderer } from "./yuvGl";
import type { Settings, Status } from "../types";

/** Settings persisted with a small debounce, and mirrored to the other windows (main + popped-out
 *  panels) through an event so an edit in one shows up in all. */
export function useSettings() {
  const [settings, setSettings] = useState<Settings | null>(null);
  const timer = useRef<number>(0);
  const me = getCurrentWindow().label;
  useEffect(() => {
    api.loadSettings().then(setSettings);
    const un = listen<{ from: string; settings: Settings }>("settings-changed", (e) => {
      if (e.payload.from !== me) setSettings(e.payload.settings);
    });
    return () => {
      un.then((f) => f());
    };
  }, []);
  const update = (patch: (s: Settings) => Settings) =>
    setSettings((cur) => {
      if (!cur) return cur;
      const next = patch(cur);
      window.clearTimeout(timer.current);
      timer.current = window.setTimeout(() => {
        api.saveSettings(next);
        emit("settings-changed", { from: me, settings: next });
      }, 250);
      return next;
    });
  return [settings, update] as const;
}

/** 1 Hz status poll; paused while the window is hidden. */
export function useStatus() {
  const [status, setStatus] = useState<Status | null>(null);
  const last = useRef<string>("");
  const refresh = async () => {
    const s = await api.status();
    const sig = s ? `live rec=${s.recording} ch=${s.channels.map((c) => c.state).join(",")}` : "null";
    if (sig !== last.current) {
      last.current = sig;
      void api.debugLog(`status -> ${sig}`);
    }
    setStatus(s);
  };
  useEffect(() => {
    refresh();
    const t = window.setInterval(() => {
      if (!document.hidden) refresh();
    }, 1000);
    return () => window.clearInterval(t);
  }, []);
  return [status, refresh] as const;
}

export function useNow(active: boolean) {
  const [now, setNow] = useState(Date.now());
  useEffect(() => {
    if (!active) return;
    const t = window.setInterval(() => setNow(Date.now()), 1000);
    return () => window.clearInterval(t);
  }, [active]);
  return now;
}

export function useDiskFree(path: string | undefined) {
  const [free, setFree] = useState<number | null>(null);
  useEffect(() => {
    if (!path) return;
    const read = () => api.diskFree(path).then(setFree);
    read();
    const t = window.setInterval(read, 30_000);
    return () => window.clearInterval(t);
  }, [path]);
  return free;
}

/** Draws a channel's live preview on a <canvas> with WebGL, always showing the newest frame.
 *  Raw YUV frames arrive over Tauri's IPC and are converted to RGB on the GPU; frames that come
 *  faster than the display can paint are dropped, so latency never builds up (the same idea as
 *  OBS' preview). Returns whether a first frame has been shown. Stops the stream when `active`
 *  turns false or the component unmounts. */
export function useLivePreview(canvas: React.RefObject<HTMLCanvasElement | null>, id: number, active: boolean) {
  const [live, setLive] = useState(false);
  useEffect(() => {
    setLive(false);
    const c = canvas.current;
    void api.debugLog(`CH${id} preview effect: active=${active} canvas=${c ? "yes" : "no"}`);
    if (!active || !c) return;
    let renderer: YuvRenderer;
    try {
      renderer = new YuvRenderer(c);
    } catch (e) {
      api.debugLog(`CH${id}: GPU preview unavailable: ${String(e)}`);
      return;
    }
    let stop = false;
    let unwatch: (() => void) | null = null;
    let latest: Uint8Array | null = null;
    let raf = 0;
    let shown = false;
    let painted = 0;
    let received = 0;
    let lagSum = 0;
    let lagMax = 0;
    let statTimer = 0;
    api.debugEnabled().then((on) => {
      if (!on || stop) return;
      selfTest().forEach((l) => api.debugLog(l)); // once per mount; colour-checks every shader path
      const t0 = performance.now();
      statTimer = window.setInterval(() => {
        const s = (performance.now() - t0) / 1000;
        api.debugLog(
          `CH${id} preview: received ${received}, painted ${painted} in ${s.toFixed(1)}s => ${(painted / s).toFixed(1)} fps; relay->paint latency avg ${(lagSum / Math.max(1, painted)).toFixed(1)} ms, max ${lagMax} ms`
        );
      }, 3000);
    });

    const paint = () => {
      raf = requestAnimationFrame(paint);
      if (!latest) return;
      const msg = latest;
      latest = null; // newest frame wins; older ones were already dropped
      const r = renderer.draw(msg);
      if (!r) return;
      painted++;
      const lag = Math.max(0, Date.now() - r.sentMs);
      lagSum += lag;
      lagMax = Math.max(lagMax, lag);
      if (!shown) {
        shown = true;
        setLive(true);
      }
    };
    raf = requestAnimationFrame(paint);

    api
      .watchPreview(id, (frame) => {
        received++;
        latest = frame;
      })
      .then((u) => (stop ? u() : (unwatch = u)));

    return () => {
      stop = true;
      unwatch?.();
      cancelAnimationFrame(raf);
      window.clearInterval(statTimer);
      renderer.dispose();
    };
  }, [id, active, canvas]);
  return live;
}
