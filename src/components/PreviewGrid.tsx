import { useRef } from "react";
import clsx from "clsx";
import { VideoOff } from "lucide-react";
import { useLivePreview } from "../lib/hooks";
import { useApp } from "../lib/ctx";
import type { ChannelSetting, ChannelStatus } from "../types";
import { OverlayLayer } from "./OverlayLayer";
import { Panel } from "./Panel";

function Tile({ id, setting, status, active, editable }: { id: number; setting: ChannelSetting; status?: ChannelStatus; active: boolean; editable: boolean }) {
  const watching = active && setting.enabled;
  const canvas = useRef<HTMLCanvasElement>(null);
  const loaded = useLivePreview(canvas, id, watching);
  const warn = status && (status.state === "restarting" || status.state === "no_signal" || status.dropped > 0);
  return (
    <div className={clsx("relative flex items-center justify-center bg-black rounded border overflow-hidden aspect-video",
      status?.state === "restarting" ? "border-orange-500" : status?.state === "no_signal" ? "border-red-500" : "border-border")}>
      {watching && <canvas ref={canvas} className={clsx("w-full h-full object-contain", !loaded && "hidden")} />}
      {status?.state === "no_signal" && <div className="absolute inset-0 flex items-center justify-center bg-black/70 text-red-400 text-sm font-semibold">NO SIGNAL</div>}
      {!(watching && loaded) && <div className="flex flex-col items-center gap-2 text-muted text-xs"><VideoOff size={22} />{setting.enabled ? (active ? "Waiting for video…" : "Preview off") : "Disabled"}</div>}
      <OverlayLayer channel={id} editable={editable} />
      <span className="absolute top-1.5 left-1.5 rounded bg-black/60 px-1.5 py-0.5 text-[11px] font-semibold">CH{id}</span>
      {status && (
        <span className={clsx("absolute bottom-1.5 left-1.5 rounded bg-black/60 px-1.5 py-0.5 text-[11px] font-mono", warn && "text-orange-400")}>
          {status.state === "restarting" ? "reconnecting…" : status.state === "no_signal" ? "no signal" : `${status.speed.toFixed(2)}x · drop ${status.dropped} · ${status.cpu_percent.toFixed(0)}% cpu`}
        </span>
      )}
    </div>
  );
}

export function PreviewGrid() {
  const { settings, statuses, recording, live } = useApp();
  const preview = settings.preview;
  const shown = settings.channels.map((c, i) => ({ c, id: i + 1 })).filter(({ c }) => c.enabled);
  const cols = shown.length <= 1 ? "grid-cols-1" : shown.length <= 4 ? "grid-cols-2" : "grid-cols-3";
  return (
    <Panel className="flex flex-col">
      <div className="p-2 overflow-auto flex-1 min-h-0">
      {!preview && recording && <div className="text-xs text-muted pb-2">Preview is off (saves CPU). Turn it on in Recording.</div>}
      <div className={clsx("grid gap-2 content-start", cols)}>
        {shown.map(({ c, id }) => (
          <Tile key={id} id={id} setting={c} status={statuses.find((s) => s.id === id)} active={live && preview} editable={id === shown[0].id} />
        ))}
      </div>
      </div>
    </Panel>
  );
}
