import { useState } from "react";
import { Bold, Clock, Image as ImageIcon, Radio, Trash2, Type } from "lucide-react";
import clsx from "clsx";
import { api } from "../lib/api";
import { useApp } from "../lib/ctx";
import { MAX_ITEMS, newItem } from "../lib/overlay";
import type { OverlayItem } from "../types";

const btn = "inline-flex items-center gap-1.5 h-7 px-2.5 rounded border border-[#3c404d] bg-[#323540] text-xs hover:bg-[#3c404d] disabled:opacity-40 disabled:pointer-events-none";
const field = "h-7 rounded bg-surface-3 border border-border px-2 text-xs text-white focus:outline-none focus:border-accent";

/** The overlay tool tab: add Text / Image / Live data / Time, then tune the selected item. */
export function OverlayToolbar() {
  const { settings, update, values, selected, setSelected, recording, overlayEditable } = useApp();
  const locked = !overlayEditable;
  const [liveOpen, setLiveOpen] = useState(false);
  const ov = settings.overlay;
  const items = ov.items;
  const full = items.length >= MAX_ITEMS;
  const keys = Object.keys(values).filter((k) => !k.startsWith("_"));
  const sel = items.find((i) => i.id === selected) ?? null;

  const add = (it: OverlayItem) => {
    update((s) => ({ ...s, overlay: { ...s.overlay, enabled: true, items: [...s.overlay.items, it] } }));
    setSelected(it.id);
  };
  const patch = (p: Partial<OverlayItem>) =>
    sel && update((s) => ({ ...s, overlay: { ...s.overlay, items: s.overlay.items.map((i) => (i.id === sel.id ? { ...i, ...p } : i)) } }));
  const remove = () => {
    if (!sel) return;
    update((s) => ({ ...s, overlay: { ...s.overlay, items: s.overlay.items.filter((i) => i.id !== sel.id) } }));
    setSelected(null);
  };
  const addImage = async () => {
    const path = await api.pickImage();
    if (path) add(newItem("image", { image: path, x: 0.75, y: 0.05 }));
  };

  return (
    <div className="shrink-0 border-b border-[#3c404d] bg-[#272a33]">
      <div className="flex items-center gap-1.5 px-2 py-1.5 flex-wrap">
        <button className={btn} disabled={full || locked} onClick={() => add(newItem("text"))}><Type size={13} /> Text</button>
        <button className={btn} disabled={full || locked} onClick={addImage}><ImageIcon size={13} /> Image</button>
        <div className="relative">
          <button className={btn} disabled={full || locked} onClick={() => setLiveOpen(!liveOpen)}><Radio size={13} /> Live data</button>
          {liveOpen && (
            <>
              <div className="fixed inset-0 z-40" onClick={() => setLiveOpen(false)} />
              <div className="absolute left-0 top-8 z-50 w-48 max-h-60 overflow-auto rounded border border-border bg-surface-2 py-1 shadow-xl">
                {keys.length === 0 && <div className="px-3 py-2 text-xs text-muted">No live data yet. Choose a data source in the Overlay panel.</div>}
                {keys.map((k) => (
                  <button key={k} className="flex w-full justify-between gap-3 px-3 py-1.5 text-left text-xs hover:bg-surface-3"
                    onClick={() => { add(newItem("text", { text: `${k} {${k}}`, bg_opacity: 0.5 })); setLiveOpen(false); }}>
                    <span className="font-mono">{k}</span><span className="text-muted">{values[k]}</span>
                  </button>
                ))}
              </div>
            </>
          )}
        </div>
        <button className={btn} disabled={full || locked} onClick={() => add(newItem("text", { text: "{time}", bg_opacity: 0.5 }))}><Clock size={13} /> Time</button>
        <span className="text-[11px] text-muted font-mono">{items.length}/{MAX_ITEMS}</span>
        <div className="flex-1" />
        {recording && locked && <span className="text-[11px] text-orange-400">Locked while recording (enable overlay before Start, in Layer mode)</span>}
        <label className="flex items-center gap-1.5 text-xs">
          <input type="checkbox" className="accent-[#4f8ef7]" checked={ov.enabled}
            onChange={(e) => update((s) => ({ ...s, overlay: { ...s.overlay, enabled: e.target.checked } }))} />
          Burn into video
        </label>
      </div>

      {sel && (
        <div className={clsx("flex items-center gap-2 px-2 pb-1.5 flex-wrap", locked && "opacity-50 pointer-events-none")}>
          {sel.kind === "text" ? (
            <>
              <input className={clsx(field, "w-44 font-mono")} value={sel.text} title="Use {name} for live values" onChange={(e) => patch({ text: e.target.value })} />
              <label className="flex items-center gap-1 text-xs text-muted">Size
                <input type="range" min={0.02} max={0.2} step={0.005} value={sel.size} onChange={(e) => patch({ size: +e.target.value })} className="w-20 accent-[#4f8ef7]" /></label>
              <input type="color" title="Text color" className="h-7 w-8 rounded bg-transparent border border-border" value={sel.color} onChange={(e) => patch({ color: e.target.value })} />
              <button title="Bold" className={clsx(btn, "w-7 px-0 justify-center", sel.bold && "!bg-accent !border-accent")} onClick={() => patch({ bold: !sel.bold })}><Bold size={13} /></button>
              <input type="color" title="Background color" className="h-7 w-8 rounded bg-transparent border border-border" value={sel.bg_color} onChange={(e) => patch({ bg_color: e.target.value })} />
              <label className="flex items-center gap-1 text-xs text-muted">Bg
                <input type="range" min={0} max={1} step={0.05} value={sel.bg_opacity} onChange={(e) => patch({ bg_opacity: +e.target.value })} className="w-16 accent-[#4f8ef7]" /></label>
            </>
          ) : (
            <>
              <button className={btn} onClick={async () => { const p = await api.pickImage(); if (p) patch({ image: p }); }}>Change image</button>
              <label className="flex items-center gap-1 text-xs text-muted">Size
                <input type="range" min={0.03} max={1} step={0.01} value={sel.width} onChange={(e) => patch({ width: +e.target.value })} className="w-24 accent-[#4f8ef7]" /></label>
              <label className="flex items-center gap-1 text-xs text-muted">Opacity
                <input type="range" min={0.1} max={1} step={0.05} value={sel.opacity} onChange={(e) => patch({ opacity: +e.target.value })} className="w-20 accent-[#4f8ef7]" /></label>
            </>
          )}
          <button title="Delete" className={clsx(btn, "w-7 px-0 justify-center text-red-400")} onClick={remove}><Trash2 size={13} /></button>
        </div>
      )}
    </div>
  );
}
