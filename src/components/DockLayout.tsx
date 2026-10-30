import { useEffect, useRef, useState } from "react";
import { DockviewReact, getPanelData, themeAbyssSpaced } from "dockview-react";
import type { DockviewApi, DockviewGroupPanel, DockviewReadyEvent, IDockviewHeaderActionsProps, IDockviewPanelHeaderProps } from "dockview-react";
import { listen } from "@tauri-apps/api/event";
import "dockview-react/dist/styles/dockview.css";
import { AppWindow, ExternalLink, LayoutGrid, Lock, LockOpen, RotateCcw } from "lucide-react";
import { api as backend } from "../lib/api";
import clsx from "clsx";
import { ControlsPanel } from "./ControlsPanel";
import { OverlayPanel } from "./OverlayPanel";
import { OverlayToolbar } from "./OverlayToolbar";
import { Panel } from "./Panel";
import { PreviewGrid } from "./PreviewGrid";
import { SettingsPanel } from "./SettingsPanel";
import { SourcesPanel } from "./SourcesPanel";

export const PANELS = [
  { id: "sources", title: "Sources", component: SourcesPanel, size: [340, 520] },
  { id: "preview", title: "Preview", component: PreviewGrid, size: [760, 480] },
  { id: "recording", title: "Recording", component: SettingsPanel, size: [360, 560] },
  { id: "tools", title: "Overlay Tools", component: ToolsPanel, size: [640, 140] },
  { id: "overlay", title: "Overlay", component: OverlayPanel, size: [380, 560] },
  { id: "controls", title: "Controls", component: ControlsPanel, size: [300, 200] },
] as const;
type PanelId = (typeof PANELS)[number]["id"];

// v2: layouts saved before the spaced theme carry no gaps.
const LAYOUT_KEY = "xspector-record.layout.v6";
export const LOCK_KEY = "xspector-record.locked";
const THEME = { ...themeAbyssSpaced, gap: 4 };
function ToolsPanel() {
  return (
    <Panel>
      <OverlayToolbar />
    </Panel>
  );
}

const components = Object.fromEntries(PANELS.map((p) => [p.id, p.component]));
/** Panels currently living in their own OS window (so the View menu can show them). */
const undocked = new Set<PanelId>();

function addPanel(api: DockviewApi, id: PanelId) {
  const title = PANELS.find((p) => p.id === id)!.title;
  const at = (ref: PanelId, direction: "left" | "right" | "above" | "below" | "within", size: number) => {
    const referencePanel = api.getPanel(ref);
    if (!referencePanel) return api.addPanel({ id, component: id, title });
    const horizontal = direction === "left" || direction === "right";
    api.addPanel({
      id, component: id, title,
      position: { referencePanel, direction },
      ...(direction === "within" ? {} : horizontal ? { initialWidth: size } : { initialHeight: size }),
    });
  };
  switch (id) {
    case "sources": return at("preview", "left", 280);
    case "recording": return at("preview", "right", 290);
    // OBS (setCorner): side docks own the corners, so a bottom dock sits between them.
    case "tools": return at("preview", "above", 96);
    case "overlay": return at("recording", "within", 0); // tabbed next to Recording
    case "controls": return at("preview", "below", 150);
    default: return api.addPanel({ id, component: id, title });
  }
}

function applyDefault(api: DockviewApi) {
  api.clear();
  undocked.forEach((id) => void apiClose(id));
  undocked.clear();
  (["preview", "tools", "sources", "recording", "overlay", "controls"] as const).forEach((id) => addPanel(api, id));
}

const apiClose = (id: PanelId) => backend.closePanelWindow(id);

async function popOut(api: DockviewApi, id: PanelId) {
  const def = PANELS.find((p) => p.id === id)!;
  await backend.openPanelWindow(id, def.title, def.size[0], def.size[1]);
  undocked.add(id);
  const panel = api.getPanel(id);
  if (panel) api.removePanel(panel);
}

/** OBS-style dock title: small dock icon + bold name. Dragging it moves the dock, double-click floats it. */
function DockTab({ api, containerApi }: IDockviewPanelHeaderProps) {
  return (
    <div
      className="flex items-center gap-2 h-full px-3 text-[12px] font-semibold text-gray-100"
      onDoubleClick={() => popOut(containerApi, api.id as PanelId)}
    >
      <AppWindow size={14} className="text-gray-400" />
      {api.title}
    </div>
  );
}

/** Header button on every group: move the active panel into its own window. */
function PopOut({ containerApi, activePanel }: IDockviewHeaderActionsProps) {
  if (!activePanel) return null;
  return (
    <button
      title="Float in its own window (or double-click the title)"
      className="h-full px-2 text-gray-400 hover:text-white"
      onClick={() => popOut(containerApi, activePanel.id as PanelId)}
    >
      <ExternalLink size={13} />
    </button>
  );
}

/** OBS drags the whole dock with its contents. dockview only drags a small label, so replace the
 *  drag image with a snapshot of the dock under the cursor. Runs after dockview's own dragstart
 *  handler (bubbling vs target phase), so the last setDragImage call is ours. */
function useDockDragImage() {
  useEffect(() => {
    const onDragStart = (e: DragEvent) => {
      const target = e.target as HTMLElement | null;
      const group = target?.closest?.(".dv-groupview") as HTMLElement | null;
      if (!group || !e.dataTransfer) return;
      const rect = group.getBoundingClientRect();
      const w = Math.min(rect.width, 420);
      const h = Math.min(rect.height, 300);
      const ghost = group.cloneNode(true) as HTMLElement;
      Object.assign(ghost.style, {
        position: "fixed", left: "-10000px", top: "0", width: `${w}px`, height: `${h}px`,
        overflow: "hidden", opacity: "0.85", pointerEvents: "none",
        boxShadow: "0 8px 24px rgba(0,0,0,.5)", borderRadius: "4px",
      });
      document.body.appendChild(ghost);
      const ox = Math.min(Math.max(e.clientX - rect.left, 12), w - 12);
      const oy = Math.min(Math.max(e.clientY - rect.top, 12), h - 12);
      e.dataTransfer.setDragImage(ghost, ox, oy);
      setTimeout(() => ghost.remove(), 0);
    };
    document.addEventListener("dragstart", onDragStart);
    return () => document.removeEventListener("dragstart", onDragStart);
  }, []);
}

type Zone = "top" | "bottom" | "left" | "right";

/** OBS (Qt) docks next to / above / below the dock under the cursor and only tabs when you drop on
 *  the title bar. dockview tabs on the whole middle of a dock, which made "swap top and bottom"
 *  impossible. This replaces the content-area drop zones: nearest edge wins, header = tab. */
function installDropZones(api: DockviewApi): () => void {
  const HEADER = 28;
  const box = document.createElement("div");
  box.className = "xs-drop-indicator";
  document.body.appendChild(box);
  type Pending = { group: DockviewGroupPanel; zone: Zone } | null;
  let pending = null as Pending;
  let committed = null as Pending;

  const clear = () => {
    pending = null;
    box.style.display = "none";
    document.documentElement.classList.remove("xs-own-dnd");
  };

  const onOver = (e: DragEvent) => {
    const data = getPanelData();
    if (!data) return clear();
    const group = api.groups.find((g) => {
      const r = g.element.getBoundingClientRect();
      return e.clientX >= r.left && e.clientX <= r.right && e.clientY >= r.top && e.clientY <= r.bottom;
    });
    if (!group) return clear();
    const r = group.element.getBoundingClientRect();
    const onlyPanelOfSameGroup = data.groupId === group.id && group.panels.length <= 1;
    if (e.clientY - r.top < HEADER || onlyPanelOfSameGroup) return clear(); // title bar: dockview tabs it
    const fx = (e.clientX - r.left) / r.width;
    const fy = (e.clientY - r.top - HEADER) / Math.max(1, r.height - HEADER);
    const d: [Zone, number][] = [["left", fx], ["right", 1 - fx], ["top", fy], ["bottom", 1 - fy]];
    const zone = d.reduce((a, b) => (b[1] < a[1] ? b : a))[0];
    pending = { group, zone };
    const half = (v: number) => `${v / 2}px`;
    Object.assign(box.style, {
      display: "block",
      left: `${zone === "right" ? r.left + r.width / 2 : r.left}px`,
      top: `${zone === "bottom" ? r.top + r.height / 2 : r.top}px`,
      width: zone === "left" || zone === "right" ? half(r.width) : `${r.width}px`,
      height: zone === "top" || zone === "bottom" ? half(r.height) : `${r.height}px`,
    });
    document.documentElement.classList.add("xs-own-dnd"); // hide dockview's own overlay
  };

  // Capture phase: runs before dockview's drop handler so onWillDrop can see the decision.
  const onDrop = () => {
    committed = pending;
    const data = getPanelData();
    const target = committed;
    clear();
    if (!target || !data) return;
    setTimeout(() => {
      const move = { group: target.group, position: target.zone };
      const panel = data.panelId ? api.getPanel(data.panelId) : undefined;
      if (panel) panel.api.moveTo(move);
      else api.getGroup(data.groupId)?.api.moveTo(move);
      committed = null;
    }, 0);
  };
  const will = api.onWillDrop((ev) => {
    if (committed) ev.preventDefault(); // we place it ourselves
  });

  document.addEventListener("dragover", onOver);
  document.addEventListener("drop", onDrop, true);
  document.addEventListener("dragend", clear);
  return () => {
    will.dispose();
    document.removeEventListener("dragover", onOver);
    document.removeEventListener("drop", onDrop, true);
    document.removeEventListener("dragend", clear);
    box.remove();
  };
}

/** Qt lets you grab the point where two separators cross and resize both at once. dockview's
 *  separators are independent, so overlay a small handle on each crossing and forward the press to
 *  every separator that meets there; their own document-level move handlers then do the work. */
function CornerHandles({ api, rootRef }: { api: DockviewApi | null; rootRef: React.RefObject<HTMLDivElement | null> }) {
  const [pts, setPts] = useState<{ x: number; y: number; sashes: HTMLElement[] }[]>([]);
  useEffect(() => {
    const root = rootRef.current;
    if (!api || !root) return;
    let raf = 0;
    const compute = () => {
      raf = 0;
      const base = root.getBoundingClientRect();
      const rects = [...root.querySelectorAll<HTMLElement>(".dv-sash")].map((el) => ({ el, r: el.getBoundingClientRect() }));
      const tall = rects.filter(({ r }) => r.height > r.width * 2); // separators between columns
      const wide = rects.filter(({ r }) => r.width > r.height * 2); // separators between rows
      const found: { x: number; y: number; sashes: HTMLElement[] }[] = [];
      const TOL = 8;
      for (const a of tall) {
        const ax = a.r.left + a.r.width / 2;
        for (const b of wide) {
          const by = b.r.top + b.r.height / 2;
          const touches = Math.abs(b.r.right - ax) < TOL + a.r.width || Math.abs(b.r.left - ax) < TOL + a.r.width;
          if (!touches || by < a.r.top || by > a.r.bottom) continue;
          const near = found.find((p) => Math.abs(p.x - (ax - base.left)) < 2 && Math.abs(p.y - (by - base.top)) < 6);
          if (near) near.sashes.push(b.el);
          else found.push({ x: ax - base.left, y: by - base.top, sashes: [a.el, b.el] });
        }
      }
      setPts(found);
    };
    const schedule = () => { if (!raf) raf = requestAnimationFrame(compute); };
    const sub = api.onDidLayoutChange(schedule);
    const ro = new ResizeObserver(schedule);
    ro.observe(root);
    window.addEventListener("pointerup", schedule);
    schedule();
    return () => {
      sub.dispose();
      ro.disconnect();
      window.removeEventListener("pointerup", schedule);
      cancelAnimationFrame(raf);
    };
  }, [api, rootRef]);

  return (
    <div className="absolute inset-0 pointer-events-none z-30">
      {pts.map((p, i) => (
        <div
          key={i}
          className="xs-corner-handle"
          style={{ left: p.x - 8, top: p.y - 8 }}
          onPointerDown={(e) => {
            e.preventDefault();
            for (const el of p.sashes) {
              el.dispatchEvent(new PointerEvent("pointerdown", { clientX: e.clientX, clientY: e.clientY, pointerId: e.pointerId, bubbles: true, button: 0 }));
            }
          }}
        />
      ))}
    </div>
  );
}

export function DockLayout({ onApi, locked }: { onApi: (api: DockviewApi) => void; locked: boolean }) {
  useDockDragImage();
  const rootRef = useRef<HTMLDivElement>(null);
  const [dockApi, setDockApi] = useState<DockviewApi | null>(null);
  const onReady = (e: DockviewReadyEvent) => {
    const api = e.api;
    installDropZones(api);
    setDockApi(api);
    try {
      const saved = localStorage.getItem(LAYOUT_KEY);
      if (!saved) throw new Error("no saved layout");
      api.fromJSON(JSON.parse(saved));
    } catch {
      applyDefault(api);
    }
    let t = 0;
    api.onDidLayoutChange(() => {
      window.clearTimeout(t);
      t = window.setTimeout(() => localStorage.setItem(LAYOUT_KEY, JSON.stringify(api.toJSON())), 400);
    });
    // Closing a popped-out window puts the panel back into the dock.
    listen<string>("panel-closed", (ev) => {
      const id = ev.payload as PanelId;
      undocked.delete(id);
      if (!api.getPanel(id)) addPanel(api, id);
    });
    onApi(api);
  };
  return (
    <div ref={rootRef} className="relative h-full">
      <DockviewReact
        className="xspector-dock"
        theme={THEME}
        components={components}
        defaultTabComponent={DockTab}
        singleTabMode="fullwidth"
        disableDnd={locked}
        rightHeaderActionsComponent={locked ? undefined : PopOut}
        onReady={onReady}
      />
      {!locked && <CornerHandles api={dockApi} rootRef={rootRef} />}
    </div>
  );
}

/** "View" menu: show/hide each window and reset the layout, like OBS' Docks menu. */
export function ViewMenu({ api, locked, onLock }: { api: DockviewApi | null; locked: boolean; onLock: (v: boolean) => void }) {
  const [open, setOpen] = useState(false);
  const [, force] = useState(0);
  useEffect(() => {
    if (!api) return;
    const subs = [api.onDidAddPanel(() => force((n) => n + 1)), api.onDidRemovePanel(() => force((n) => n + 1))];
    return () => subs.forEach((s) => s.dispose());
  }, [api]);
  if (!api) return null;
  const toggle = (id: PanelId) => {
    if (undocked.has(id)) return void apiClose(id); // closing its window re-docks it
    const p = api.getPanel(id);
    if (p) api.removePanel(p);
    else addPanel(api, id);
  };
  return (
    <div className="relative">
      <button onClick={() => setOpen(!open)} className="inline-flex items-center gap-1.5 h-8 px-3 rounded text-gray-300 hover:bg-surface-3">
        <LayoutGrid size={14} /> View
      </button>
      {open && (
        <>
          <div className="fixed inset-0 z-40" onClick={() => setOpen(false)} />
          <div className="absolute right-0 top-9 z-50 w-56 rounded border border-border bg-surface-2 py-1 shadow-xl">
            {PANELS.map((p) => (
              <button key={p.id} onClick={() => toggle(p.id)} className="flex w-full items-center gap-2 px-3 py-1.5 text-left hover:bg-surface-3">
                <span className={clsx("w-3 text-accent", !api.getPanel(p.id) && !undocked.has(p.id) && "opacity-0")}>✓</span> {p.title}
                {undocked.has(p.id) && <ExternalLink size={11} className="ml-auto text-muted" />}
              </button>
            ))}
            <div className="my-1 border-t border-border" />
            <button onClick={() => onLock(!locked)} className="flex w-full items-center gap-2 px-3 py-1.5 text-left hover:bg-surface-3">
              {locked ? <Lock size={12} /> : <LockOpen size={12} />} {locked ? "Unlock layout" : "Lock layout"}
            </button>
            <button
              onClick={() => { applyDefault(api); setOpen(false); }}
              className="flex w-full items-center gap-2 px-3 py-1.5 text-left hover:bg-surface-3"
            >
              <RotateCcw size={12} /> Reset layout
            </button>
            <div className="px-3 py-1 text-[11px] text-muted">Drag a tab to move it. ↗ pops it out into its own window; closing that window docks it back.</div>
          </div>
        </>
      )}
    </div>
  );
}
