import type { ReactNode } from "react";
import clsx from "clsx";

/** Body of a dockable window. The title/tab and the window chrome come from dockview. */
export function Panel({ toolbar, children, className }: { toolbar?: ReactNode; children: ReactNode; className?: string }) {
  return (
    <div className="flex flex-col h-full bg-[#272a33] text-sm">
      {toolbar && (
        <div className="flex items-center justify-end gap-1 px-2 h-8 shrink-0 bg-surface-2 border-b border-border">{toolbar}</div>
      )}
      <div className={clsx("flex-1 min-h-0 overflow-auto", className)}>{children}</div>
    </div>
  );
}

export const INPUT =
  "w-full rounded bg-surface-3 border border-border px-2.5 py-1.5 text-sm text-white placeholder:text-muted focus:outline-none focus:border-accent disabled:opacity-50";
