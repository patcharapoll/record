import { createContext, useContext } from "react";
import type { ChannelSetting, ChannelStatus, Encoder, Settings, VideoDevice } from "../types";

/** Everything the dockable windows need. Dockview instantiates panels itself, so they read this. */
export interface AppCtx {
  settings: Settings;
  update: (p: (s: Settings) => Settings) => void;
  setChannel: (i: number, patch: Partial<ChannelSetting>) => void;
  addChannel: () => void;
  removeChannel: (i: number) => void;
  statuses: ChannelStatus[];
  recording: boolean;
  /** camera is being shown (live monitor or recording) */
  live: boolean;
  devices: VideoDevice[];
  encoders: Encoder[];
  refreshDevices: () => void;
  warnings: string[];
  /** overlay is a live layer in the running recording */
  layerActive: boolean;
  /** overlay can be edited right now (idle, or recording with a live layer) */
  overlayEditable: boolean;
  values: Record<string, string>;
  selected: string | null;
  setSelected: (id: string | null) => void;
  toggleRecording: () => void;
  busy: boolean;
  elapsed: number;
  error: string;
  openFolder: () => void;
}

const Ctx = createContext<AppCtx | null>(null);
export const AppProvider = Ctx.Provider;
export function useApp(): AppCtx {
  const v = useContext(Ctx);
  if (!v) throw new Error("AppProvider missing");
  return v;
}
