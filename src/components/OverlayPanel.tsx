import clsx from "clsx";
import { Eye, EyeOff, Image as ImageIcon, Trash2, Type } from "lucide-react";
import { useApp } from "../lib/ctx";
import type { DataSource, OverlayConfig } from "../types";
import { INPUT, Panel } from "./Panel";

/** Data source and the list of overlay objects. Adding and editing happens on the Preview. */
export function OverlayPanel() {
  const { settings, update, recording, values, selected, setSelected } = useApp();
  const ov = settings.overlay;
  const set = (p: (o: OverlayConfig) => OverlayConfig) => update((s) => ({ ...s, overlay: p(s.overlay) }));
  const setSource = (kind: string) =>
    set((o) => ({ ...o, source: (kind === "udp_json" ? { kind, port: 5005 } : { kind }) as DataSource }));
  const keys = Object.keys(values).filter((k) => !k.startsWith("_"));

  return (
    <Panel className={clsx("p-3 space-y-4", recording && "opacity-60 pointer-events-none")}>
      <div className="space-y-1">
        <span className="text-xs text-muted">Overlay mode</span>
        <select className={INPUT} value={ov.mode} onChange={(e) => set((o) => ({ ...o, mode: e.target.value as OverlayConfig["mode"] }))}>
          <option value="layer">Live — editable while recording (default)</option>
          <option value="burn">Fixed — drawn by FFmpeg, locked once recording starts</option>
        </select>
        <p className="text-[11px] text-muted">
          Live is blended by the app into every frame, so you can move and edit objects while recording. Both
          modes cost about the same CPU (measured on a 720p camera). Turn on “Burn into video” before pressing Start.
        </p>
      </div>

      <div className="space-y-1">
        <span className="text-xs text-muted">Live data source</span>
        <select className={INPUT} value={ov.source.kind} onChange={(e) => setSource(e.target.value)}>
          <option value="none">None (time only)</option>
          <option value="simulator">Simulator (fake depth / heading)</option>
          <option value="udp_json">UDP — JSON per packet</option>
        </select>
        {ov.source.kind === "udp_json" && (
          <input className={INPUT} type="number" min={1} max={65535} value={ov.source.port}
            onChange={(e) => set((o) => ({ ...o, source: { kind: "udp_json", port: +e.target.value || 5005 } }))} />
        )}
        <p className="text-[11px] text-muted">
          Send e.g. {`{"depth": 12.3, "heading": 270}`}. Use values in text as {"{depth}"}; built-in: {"{time} {date} {clock} {ch}"}.
        </p>
        {keys.length > 0 && (
          <div className="flex flex-wrap gap-1 pt-1">
            {keys.map((k) => (
              <span key={k} className="rounded bg-surface-3 border border-border px-1.5 py-0.5 text-[11px] font-mono">{`{${k}}`} = {values[k]}</span>
            ))}
          </div>
        )}
      </div>

      <div className="space-y-1">
        <span className="text-xs text-muted">Objects ({ov.items.length}) — add them from the toolbar on Preview</span>
        {ov.items.length === 0 && <div className="text-xs text-muted py-2">Nothing yet.</div>}
        {ov.items.map((it) => (
          <div key={it.id} onClick={() => setSelected(it.id)}
            className={clsx("flex items-center gap-2 rounded border px-2 py-1.5 cursor-pointer",
              selected === it.id ? "border-accent bg-accent/10" : "border-border bg-surface-2 hover:border-gray-500", !it.enabled && "opacity-60")}>
            {it.kind === "text" ? <Type size={13} className="text-muted shrink-0" /> : <ImageIcon size={13} className="text-muted shrink-0" />}
            <span className="flex-1 truncate text-xs font-mono">{it.kind === "text" ? it.text : (it.image ?? "").split(/[\\/]/).pop()}</span>
            <button title={it.enabled ? "Hide" : "Show"} className="text-gray-400 hover:text-white"
              onClick={(e) => { e.stopPropagation(); set((o) => ({ ...o, items: o.items.map((i) => (i.id === it.id ? { ...i, enabled: !i.enabled } : i)) })); }}>
              {it.enabled ? <Eye size={13} /> : <EyeOff size={13} />}
            </button>
            <button title="Delete" className="text-red-400 hover:bg-red-900/20 rounded p-0.5"
              onClick={(e) => { e.stopPropagation(); set((o) => ({ ...o, items: o.items.filter((i) => i.id !== it.id) })); if (selected === it.id) setSelected(null); }}>
              <Trash2 size={13} />
            </button>
          </div>
        ))}
      </div>
    </Panel>
  );
}
