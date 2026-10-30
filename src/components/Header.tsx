import { Circle } from "lucide-react";
import type { ReactNode } from "react";
import clsx from "clsx";
import { clock } from "../lib/format";

export function Header({ recording, elapsedSec, menu }: { recording: boolean; elapsedSec: number; menu?: ReactNode }) {
  return (
    <header className="flex items-center gap-4 px-4 h-10 bg-surface-1 border-b border-border shrink-0">
      <div className="font-semibold tracking-tight">Xspector Record</div>
      <div
        className={clsx(
          "flex items-center gap-2 rounded px-2.5 py-1 text-xs font-mono",
          recording ? "bg-red-500/15 text-red-400" : "bg-surface-3 text-muted"
        )}
      >
        <Circle size={8} className={clsx("fill-current", recording && "animate-pulse")} />
        {recording ? `REC ${clock(elapsedSec)}` : "Ready"}
      </div>
      <div className="flex-1" />
      {menu}
    </header>
  );
}
