import { useEffect, useState } from "react";
import type { DockviewApi } from "dockview-react";
import { api } from "./lib/api";
import { AppProvider } from "./lib/ctx";
import { useAppState } from "./lib/useAppCtx";
import { Header } from "./components/Header";
import { DockLayout, LOCK_KEY, PANELS, ViewMenu } from "./components/DockLayout";
import { StatusBar } from "./components/StatusBar";

/** A popped-out panel: same UI, one panel, own OS window. */
function PanelWindow({ id }: { id: string }) {
  const { ctx } = useAppState();
  const Body = PANELS.find((p) => p.id === id)?.component;
  if (!ctx || !Body) return null;
  return (
    <AppProvider value={ctx}>
      <div className="h-screen bg-[#1d1f26] p-1">
        <div className="h-full rounded border border-[#3c404d] overflow-hidden bg-[#272a33]">
          <Body />
        </div>
      </div>
    </AppProvider>
  );
}

function Main() {
  const { ctx, settings, status, recording, elapsed, free, error } = useAppState(true);
  const [dock, setDock] = useState<DockviewApi | null>(null);
  // Keep the live monitor (camera preview without recording) in step with the settings.
  const monitorKey = settings ? JSON.stringify({ c: settings.channels, o: settings.overlay.source, p: settings.preview, w: settings.preview_width }) : "";
  useEffect(() => {
    if (!settings || recording) return;
    const t = window.setTimeout(() => api.setMonitor(settings).catch((e) => void api.debugLog(`set_monitor failed: ${String(e)}`)), 600);
    return () => window.clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [monitorKey, recording]);
  // While recording with a live overlay layer, send every edit to the running recorder.
  const overlayKey = settings ? JSON.stringify(settings.overlay) : "";
  useEffect(() => {
    if (!settings || !recording || !status?.layer_active) return;
    const t = window.setTimeout(() => api.updateOverlay(settings.overlay).catch(() => {}), 120);
    return () => window.clearTimeout(t);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [overlayKey, recording]);
  const [locked, setLocked] = useState(() => localStorage.getItem(LOCK_KEY) === "1");
  if (!ctx || !settings) return null;
  const setLock = (v: boolean) => {
    localStorage.setItem(LOCK_KEY, v ? "1" : "0");
    setLocked(v);
  };

  return (
    <AppProvider value={ctx}>
      <div className="flex flex-col h-screen bg-surface-0 text-gray-100 font-sans text-sm">
        <Header recording={recording} elapsedSec={elapsed} menu={<ViewMenu api={dock} locked={locked} onLock={setLock} />} />
        <main className="flex-1 min-h-0 p-1 bg-[#1d1f26]">
          <DockLayout onApi={setDock} locked={locked} />
        </main>
        <StatusBar
          status={status}
          elapsedSec={elapsed}
          segmentSec={settings.segment_minutes * 60}
          free={free}
          error={error}
          onOpen={ctx.openFolder}
        />
      </div>
    </AppProvider>
  );
}

export default function App() {
  const panel = new URLSearchParams(window.location.search).get("panel");
  return panel ? <PanelWindow id={panel} /> : <Main />;
}
