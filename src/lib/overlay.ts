import { useEffect, useState } from "react";
import { api } from "./api";
import type { OverlayItem } from "../types";

export const MAX_ITEMS = 30;

export function newItem(kind: "text" | "image", patch: Partial<OverlayItem> = {}): OverlayItem {
  return {
    id: crypto.randomUUID(),
    enabled: true,
    kind,
    x: 0.35 + Math.random() * 0.1,
    y: 0.4 + Math.random() * 0.1,
    text: "Text",
    size: 0.05,
    color: "#FFFFFF",
    bold: false,
    bg_color: "#000000",
    bg_opacity: 0,
    image: null,
    width: 0.2,
    opacity: 1,
    ...patch,
  };
}

const p2 = (n: number) => String(n).padStart(2, "0");

/** Same placeholders as the recorder: {key}, {time}, {date}, {clock}, {ch}. Missing keys show "--". */
export function renderTemplate(t: string, ch: number, values: Record<string, string>): string {
  const d = new Date();
  const date = `${d.getFullYear()}-${p2(d.getMonth() + 1)}-${p2(d.getDate())}`;
  const clock = `${p2(d.getHours())}:${p2(d.getMinutes())}:${p2(d.getSeconds())}`;
  return t.replace(/\{([^{}]+)\}/g, (_, raw: string) => {
    const k = raw.trim();
    if (k === "time") return `${date} ${clock}`;
    if (k === "date") return date;
    if (k === "clock") return clock;
    if (k === "ch") return String(ch);
    return values[k] ?? "--";
  });
}

const cache = new Map<string, string>();

/** Object URL for a picture on disk, cached per path. */
export function useImageUrl(path: string | null): string | null {
  const [url, setUrl] = useState<string | null>(path ? (cache.get(path) ?? null) : null);
  useEffect(() => {
    if (!path) return setUrl(null);
    const hit = cache.get(path);
    if (hit) return setUrl(hit);
    let gone = false;
    api
      .readImage(path)
      .then((buf) => {
        const u = URL.createObjectURL(new Blob([buf]));
        cache.set(path, u);
        if (!gone) setUrl(u);
      })
      .catch(() => !gone && setUrl(null));
    return () => {
      gone = true;
    };
  }, [path]);
  return url;
}
