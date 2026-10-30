import { clock, ENCODER_LABEL, gb } from "../lib/format";
import type { Status } from "../types";

export function StatusBar({ status, elapsedSec, segmentSec, free, onOpen, error }: {
  status: Status | null; elapsedSec: number; segmentSec: number; free: number | null; onOpen: () => void; error: string;
}) {
  const ch = status?.channels ?? [];
  const cpu = ch.reduce((a, c) => a + c.cpu_percent, 0);
  const ram = ch.reduce((a, c) => a + c.rss_mb, 0);
  const drop = ch.reduce((a, c) => a + c.dropped, 0);
  const files = ch.reduce((a, c) => a + c.segments, 0);
  const next = segmentSec - (elapsedSec % segmentSec);
  return (
    <footer className="shrink-0 border-t border-border bg-surface-1">
      {error && <div className="px-4 py-1.5 text-xs text-red-400 bg-red-900/20 whitespace-pre-wrap">{error}</div>}
      <div className="flex items-center gap-5 px-4 h-8 text-xs font-mono text-gray-400">
        {status && !status.recording ? (
          <span>Live monitor · CPU {cpu.toFixed(0)}% · RAM {ram.toFixed(0)} MB</span>
        ) : status ? (
          <>
            <span>CPU {cpu.toFixed(0)}%</span>
            <span>RAM {ram.toFixed(0)} MB</span>
            <span className={drop > 0 ? "text-orange-400" : ""}>Dropped {drop}</span>
            <span>Files {files}</span>
            <span>Next file in {clock(next)}</span>
            <span className="truncate">{ENCODER_LABEL[status.encoder] ?? status.encoder}</span>
          </>
        ) : <span>Idle</span>}
        <div className="flex-1" />
        <span>Disk free {gb(free)}</span>
        <button onClick={onOpen} className="text-accent hover:underline">Open folder</button>
      </div>
    </footer>
  );
}
