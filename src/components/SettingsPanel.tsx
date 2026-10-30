import { FolderOpen } from "lucide-react";
import clsx from "clsx";
import { api } from "../lib/api";
import { ENCODER_LABEL } from "../lib/format";
import { useApp } from "../lib/ctx";
import type { Encoder, LowSpaceAction, SplitLayout } from "../types";
import { INPUT, Panel } from "./Panel";

const EXAMPLE = "2025-04-09_22-13-08";

export function SettingsPanel() {
  const { settings, encoders, recording: locked, update: onChange } = useApp();
  const set = <K extends keyof typeof settings>(k: K, v: (typeof settings)[K]) => onChange((s) => ({ ...s, [k]: v }));
  const layouts: { id: SplitLayout; title: string; hint: string }[] = [
    { id: "new_folder", title: "New folder", hint: `${EXAMPLE}/CH1.mp4` },
    { id: "same_folder", title: "Same folder", hint: `${EXAMPLE}/CH1_${EXAMPLE}.mp4` },
  ];
  return (
    <Panel className={clsx("p-3 space-y-4", locked && "opacity-60 pointer-events-none")}>
      <label className="block space-y-1">
        <span className="text-xs text-muted">Save to</span>
        <div className="flex gap-1">
          <input className={INPUT} value={settings.root} onChange={(e) => set("root", e.target.value)} />
          <button title="Choose folder" className="px-2 rounded bg-surface-3 border border-border hover:border-accent"
            onClick={async () => { const d = await api.pickFolder(); if (d) set("root", d); }}>
            <FolderOpen size={15} />
          </button>
        </div>
      </label>

      <label className="block space-y-1">
        <span className="text-xs text-muted">Cut file every (minutes)</span>
        <input className={INPUT} type="number" min={1} value={settings.segment_minutes}
          onChange={(e) => set("segment_minutes", Math.max(1, +e.target.value || 30))} />
      </label>

      <div className="space-y-1">
        <span className="text-xs text-muted">When a file is cut</span>
        <div className="grid gap-1.5">
          {layouts.map((l) => (
            <button key={l.id} onClick={() => set("split_layout", l.id)}
              className={clsx("text-left rounded border px-2.5 py-1.5", settings.split_layout === l.id ? "border-accent bg-accent/10" : "border-border bg-surface-2 hover:border-gray-500")}>
              <div className="text-sm">{l.title}</div>
              <div className="text-[11px] font-mono text-muted break-all">{l.hint}</div>
            </button>
          ))}
        </div>
      </div>

      <label className="block space-y-1">
        <span className="text-xs text-muted">Encoder</span>
        <select className={INPUT} value={settings.encoder ?? ""} onChange={(e) => set("encoder", (e.target.value || null) as Encoder | null)}>
          <option value="">Auto (best available)</option>
          {encoders.map((e) => <option key={e} value={e}>{ENCODER_LABEL[e] ?? e}</option>)}
        </select>
      </label>

      <label className="block space-y-1">
        <span className="text-xs text-muted">Bitrate per channel</span>
        <input className={INPUT} value={settings.bitrate} onChange={(e) => set("bitrate", e.target.value || "4M")} />
      </label>

      <div className="space-y-2 rounded border border-border bg-surface-2 p-2.5">
        <span className="text-xs font-semibold">Storage</span>
        <label className="block space-y-1">
          <span className="text-xs text-muted">Keep at least this much free (GB, 0 = off)</span>
          <input className={INPUT} type="number" min={0} value={settings.storage.min_free_gb}
            onChange={(e) => onChange((s) => ({ ...s, storage: { ...s.storage, min_free_gb: Math.max(0, Math.round(+e.target.value || 0)) } }))} />
        </label>
        <label className="block space-y-1">
          <span className="text-xs text-muted">When space runs low</span>
          <select className={INPUT} value={settings.storage.on_low}
            onChange={(e) => onChange((s) => ({ ...s, storage: { ...s.storage, on_low: e.target.value as LowSpaceAction } }))}>
            <option value="warn">Warn only</option>
            <option value="delete_oldest">Delete the oldest recordings</option>
            <option value="stop">Stop recording</option>
          </select>
        </label>
        <label className="block space-y-1">
          <span className="text-xs text-muted">Delete recordings older than (days, 0 = keep)</span>
          <input className={INPUT} type="number" min={0} value={settings.storage.max_age_days}
            onChange={(e) => onChange((s) => ({ ...s, storage: { ...s.storage, max_age_days: Math.max(0, Math.round(+e.target.value || 0)) } }))} />
        </label>
        <p className="text-[11px] text-muted">Only files this app made in the save folder are ever deleted, never ones from the last 10 minutes.</p>
      </div>

      <label className="flex items-center gap-2">
        <input type="checkbox" className="accent-[#4f8ef7]" checked={settings.preview} onChange={(e) => set("preview", e.target.checked)} />
        <span>Show live preview <span className="text-muted text-xs">(uses CPU)</span></span>
      </label>
      {settings.preview && (
        <label className="block space-y-1">
          <span className="text-xs text-muted">Preview size (bigger = sharper, more CPU)</span>
          <select className={INPUT} value={settings.preview_width} onChange={(e) => set("preview_width", +e.target.value)}>
            <option value={640}>Light — 640 px (recommended)</option>
            <option value={960}>Medium — 960 px</option>
            <option value={1280}>Full — 1280 px</option>
          </select>
        </label>
      )}
    </Panel>
  );
}
