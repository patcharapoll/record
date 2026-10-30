import { useRef } from "react";
import clsx from "clsx";
import { useApp } from "../lib/ctx";
import { renderTemplate, useImageUrl } from "../lib/overlay";
import type { OverlayItem } from "../types";

function hexA(hex: string, a: number) {
  const h = hex.replace("#", "");
  const n = parseInt(h.length === 6 ? h : "000000", 16);
  return `rgba(${(n >> 16) & 255}, ${(n >> 8) & 255}, ${n & 255}, ${a})`;
}

/** The overlay drawn over a preview tile. On the editable tile items can be dragged and resized;
 *  the recorder burns the same items in at the same fractional positions. */
export function OverlayLayer({ channel, editable }: { channel: number; editable: boolean }) {
  const { settings, update, values, selected, setSelected, overlayEditable } = useApp();
  const box = useRef<HTMLDivElement>(null);
  const canEdit = editable && overlayEditable;
  const patch = (id: string, p: Partial<OverlayItem>) =>
    update((s) => ({ ...s, overlay: { ...s.overlay, items: s.overlay.items.map((i) => (i.id === id ? { ...i, ...p } : i)) } }));

  const drag = (e: React.PointerEvent, it: OverlayItem, mode: "move" | "resize") => {
    if (!canEdit || !box.current) return;
    e.stopPropagation();
    e.preventDefault();
    setSelected(it.id);
    const rect = box.current.getBoundingClientRect();
    const start = { px: e.clientX, py: e.clientY, x: it.x, y: it.y, w: it.width };
    const el = e.currentTarget as HTMLElement;
    el.setPointerCapture(e.pointerId);
    const move = (m: PointerEvent) => {
      const dx = (m.clientX - start.px) / rect.width;
      const dy = (m.clientY - start.py) / rect.height;
      if (mode === "move") patch(it.id, { x: Math.min(0.98, Math.max(0, start.x + dx)), y: Math.min(0.98, Math.max(0, start.y + dy)) });
      else patch(it.id, { width: Math.min(1 - it.x, Math.max(0.03, start.w + dx)) });
    };
    const up = () => {
      el.removeEventListener("pointermove", move);
      el.removeEventListener("pointerup", up);
    };
    el.addEventListener("pointermove", move);
    el.addEventListener("pointerup", up);
  };

  if (!settings.overlay.enabled) return null;
  return (
    <div ref={box} className="absolute inset-0" style={{ containerType: "size" }} onPointerDown={() => canEdit && setSelected(null)}>
      {settings.overlay.items.filter((i) => i.enabled).map((it) => (
        <Item key={it.id} it={it} channel={channel} values={values} selected={canEdit && selected === it.id} canEdit={canEdit} onDrag={drag} />
      ))}
    </div>
  );
}

function Item({ it, channel, values, selected, canEdit, onDrag }: {
  it: OverlayItem; channel: number; values: Record<string, string>; selected: boolean; canEdit: boolean;
  onDrag: (e: React.PointerEvent, it: OverlayItem, mode: "move" | "resize") => void;
}) {
  const url = useImageUrl(it.kind === "image" ? it.image : null);
  const base: React.CSSProperties = { position: "absolute", left: `${it.x * 100}%`, top: `${it.y * 100}%`, touchAction: "none" };
  const ring = clsx(canEdit && "cursor-move", selected && "outline outline-1 outline-[#476bd7]");
  if (it.kind === "image") {
    return (
      <div className={ring} style={{ ...base, width: `${it.width * 100}%`, opacity: it.opacity }} onPointerDown={(e) => onDrag(e, it, "move")}>
        {url ? <img src={url} alt="" draggable={false} className="w-full block" /> : <div className="aspect-[2/1] bg-surface-3/70 text-[10px] text-muted flex items-center justify-center">no image</div>}
        {selected && <div className="absolute -right-1 -bottom-1 w-2.5 h-2.5 bg-[#476bd7] cursor-nwse-resize" onPointerDown={(e) => onDrag(e, it, "resize")} />}
      </div>
    );
  }
  return (
    <div
      className={ring}
      onPointerDown={(e) => onDrag(e, it, "move")}
      style={{
        ...base,
        fontSize: `${it.size * 100}cqh`,
        lineHeight: 1.2,
        color: it.color,
        fontWeight: it.bold ? 700 : 400,
        whiteSpace: "pre",
        padding: it.bg_opacity > 0 ? "0.15em 0.3em" : 0,
        background: it.bg_opacity > 0 ? hexA(it.bg_color, it.bg_opacity) : "transparent",
      }}
    >
      {renderTemplate(it.text, channel, values)}
    </div>
  );
}
