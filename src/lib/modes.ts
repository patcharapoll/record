import { useEffect, useState } from "react";
import { api } from "./api";
import type { Mode } from "../types";

const cache = new Map<string, Mode[]>();

/** Sizes and frame rates a capture device offers (cached per device name for the session). */
export function useModes(device: string | null, enabled: boolean): Mode[] {
  const [modes, setModes] = useState<Mode[]>(device ? (cache.get(device) ?? []) : []);
  useEffect(() => {
    if (!device || !enabled) return setModes([]);
    const hit = cache.get(device);
    if (hit) return setModes(hit);
    let gone = false;
    api
      .deviceModes(device)
      .then((m) => {
        if (m.length) cache.set(device, m);
        if (!gone) setModes(m);
      })
      .catch(() => !gone && setModes([]));
    return () => {
      gone = true;
    };
  }, [device, enabled]);
  return modes;
}
