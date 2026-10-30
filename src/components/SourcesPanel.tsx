import { useState } from "react";
import { ChevronDown, ChevronRight, Plus, RefreshCw, Trash2 } from "lucide-react";
import clsx from "clsx";
import { useApp } from "../lib/ctx";
import type { ChannelSetting, Mode } from "../types";
import { useModes } from "../lib/modes";
import { INPUT, Panel } from "./Panel";

const TEST = "__test__";

const DOT: Record<string, string> = {
  recording: "bg-green-500",
  starting: "bg-yellow-400",
  restarting: "bg-orange-500 animate-pulse",
  no_signal: "bg-red-500 animate-pulse",
  stopped: "bg-gray-500",
};

function ModePicker({ c, i, locked, open, onChange }: {
  c: ChannelSetting; i: number; locked: boolean; open: boolean; onChange: (i: number, p: Partial<ChannelSetting>) => void;
}) {
  const modes: Mode[] = useModes(c.device, open);
  if (!c.device || modes.length === 0) {
    // Test pattern, or a device whose modes could not be read: free entry.
    return (
      <>
        <label className="space-y-1">
          <span className="text-muted">Resolution</span>
          <input className={INPUT} disabled={locked} placeholder="auto (e.g. 1280x720)" value={c.size ?? ""}
            onChange={(e) => onChange(i, { size: e.target.value || null })} />
        </label>
        <label className="space-y-1">
          <span className="text-muted">FPS</span>
          <input className={INPUT} type="number" min={1} max={120} disabled={locked} value={c.fps}
            onChange={(e) => onChange(i, { fps: Math.min(120, Math.max(1, +e.target.value || 30)) })} />
        </label>
      </>
    );
  }
  const chosen = modes.find((m) => `${m.w}x${m.h}` === c.size);
  const rates = [...new Set((chosen ? chosen.fps : modes.flatMap((m) => m.fps)).map((f) => Math.round(f)))].sort((a, b) => a - b);
  return (
    <>
      <label className="space-y-1">
        <span className="text-muted">Resolution</span>
        <select className={INPUT} disabled={locked} value={c.size ?? ""} onChange={(e) => onChange(i, { size: e.target.value || null })}>
          <option value="">Auto (best up to 720p)</option>
          {modes.map((m) => <option key={`${m.w}x${m.h}`} value={`${m.w}x${m.h}`}>{m.w}×{m.h}</option>)}
        </select>
      </label>
      <label className="space-y-1">
        <span className="text-muted">FPS</span>
        <select className={INPUT} disabled={locked} value={c.fps} onChange={(e) => onChange(i, { fps: +e.target.value })}>
          {!rates.includes(c.fps) && <option value={c.fps}>{c.fps} (not offered)</option>}
          {rates.map((f) => <option key={f} value={f}>{f}</option>)}
        </select>
      </label>
    </>
  );
}

export function SourcesPanel() {
  const { settings, statuses, devices, recording: locked, setChannel: onChange, addChannel: onAdd, removeChannel: onRemove, refreshDevices: onRefresh } = useApp();
  const channels = settings.channels;
  const [open, setOpen] = useState<number | null>(null);
  return (
    <Panel
      className="p-2 space-y-2"
      toolbar={
        <div className="flex gap-1">
          <button title="Refresh devices" onClick={onRefresh} disabled={locked} className="p-1 text-gray-400 hover:text-white disabled:opacity-40">
            <RefreshCw size={13} />
          </button>
          <button title="Add channel" onClick={onAdd} disabled={locked} className="p-1 text-gray-400 hover:text-white disabled:opacity-40">
            <Plus size={14} />
          </button>
        </div>
      }
    >
      {channels.map((c, i) => {
        const st = statuses.find((s) => s.id === i + 1);
        const value = c.device ?? TEST;
        const known = value === TEST || devices.some((d) => d.name === value);
        return (
          <div key={i} className={clsx("rounded border border-border bg-surface-2", !c.enabled && "opacity-60")}>
            <div className="flex items-center gap-2 px-2 py-1.5">
              <span className={clsx("w-2 h-2 rounded-full shrink-0", c.enabled && st ? DOT[st.state] : "bg-gray-600")} />
              <span className="text-xs font-semibold w-8">CH{i + 1}</span>
              <select
                className={clsx(INPUT, "py-1")}
                disabled={locked}
                value={value}
                onChange={(e) => onChange(i, { device: e.target.value === TEST ? null : e.target.value })}
              >
                <option value={TEST}>Test pattern (no hardware)</option>
                {!known && <option value={value}>{value} (not connected)</option>}
                {devices.map((d) => <option key={d.name}>{d.name}</option>)}
              </select>
              <input
                type="checkbox"
                title="Record this channel"
                disabled={locked}
                checked={c.enabled}
                onChange={(e) => onChange(i, { enabled: e.target.checked })}
                className="accent-[#4f8ef7]"
              />
            </div>
            {c.enabled && st?.state === "recording" && st.last_error && (
              <div className="px-2 pb-1 text-[11px] text-amber-400">{st.last_error}{st.fps ? ` · now ${st.fps} fps` : ""}</div>
            )}
            <button
              onClick={() => setOpen(open === i ? null : i)}
              className="flex items-center gap-1 px-2 pb-1.5 text-[11px] text-muted hover:text-gray-300"
            >
              {open === i ? <ChevronDown size={12} /> : <ChevronRight size={12} />} Advanced
            </button>
            {open === i && (
              <div className="grid grid-cols-2 gap-2 px-2 pb-2 text-xs">
                <ModePicker c={c} i={i} locked={locked} open={open === i} onChange={onChange} />
                <label className="col-span-2 space-y-1">
                  <span className="text-muted">Deinterlace (analog sources)</span>
                  <select className={INPUT} disabled={locked} value={c.deinterlace}
                    onChange={(e) => onChange(i, { deinterlace: e.target.value as ChannelSetting["deinterlace"] })}>
                    <option value="off">Off</option>
                    <option value="on">On — removes comb lines (about 5% CPU per channel)</option>
                  </select>
                </label>
                {channels.length > 1 && (
                  <button disabled={locked} onClick={() => onRemove(i)}
                    className="col-span-2 inline-flex items-center justify-center gap-1 text-red-400 hover:bg-red-900/20 rounded py-1 disabled:opacity-40">
                    <Trash2 size={12} /> Remove channel
                  </button>
                )}
              </div>
            )}
          </div>
        );
      })}
    </Panel>
  );
}
