import { Circle, FolderOpen, Square } from "lucide-react";
import clsx from "clsx";
import { useApp } from "../lib/ctx";
import { clock } from "../lib/format";
import { Panel } from "./Panel";

/** Like OBS' Controls dock: the buttons you press while running. */
export function ControlsPanel() {
  const { recording, busy, elapsed, error, toggleRecording, openFolder } = useApp();
  const btn = "w-full h-10 rounded border border-border bg-surface-3 hover:bg-[#2d3347] font-medium inline-flex items-center justify-center gap-2 disabled:opacity-50";
  return (
    <Panel className="p-2 space-y-2">
      <button
        onClick={toggleRecording}
        disabled={busy}
        className={clsx(btn, recording && "!bg-red-600 !border-red-600 hover:!bg-red-700 text-white")}
      >
        {recording ? <Square size={14} className="fill-current" /> : <Circle size={14} className="fill-current text-red-500" />}
        {recording ? `Stop Recording  ${clock(elapsed)}` : "Start Recording"}
      </button>
      <button onClick={openFolder} className={btn}>
        <FolderOpen size={14} /> Open Recordings Folder
      </button>
      {error && <div className="text-xs text-red-400 whitespace-pre-wrap">{error}</div>}
    </Panel>
  );
}
