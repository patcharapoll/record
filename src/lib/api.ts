import { Channel, invoke } from "@tauri-apps/api/core";
import { open } from "@tauri-apps/plugin-dialog";
import { openPath } from "@tauri-apps/plugin-opener";
import type { Encoder, Mode, Settings, Status, VideoDevice } from "../types";

export const api = {
  loadSettings: () => invoke<Settings>("load_settings"),
  saveSettings: (settings: Settings) => invoke<void>("save_settings", { settings }),
  listDevices: () => invoke<VideoDevice[]>("list_devices"),
  deviceModes: (name: string) => invoke<Mode[]>("device_modes", { name }),
  usableEncoders: () => invoke<Encoder[]>("usable_encoders"),
  start: (settings: Settings) => invoke<void>("start_recording", { settings }),
  setMonitor: (settings: Settings) => invoke<void>("set_monitor", { settings }),
  updateOverlay: (overlay: Settings["overlay"]) => invoke<void>("update_overlay", { overlay }),
  stop: () => invoke<void>("stop_recording"),
  status: () => invoke<Status | null>("status"),
  debugEnabled: () => invoke<boolean>("debug_enabled"),
  debugLog: (msg: string) => invoke<void>("debug_log", { msg }),
  /** Live JPEG frames of a channel. Returns a function that stops the stream. */
  watchPreview: async (id: number, onFrame: (msg: Uint8Array) => void) => {
    const ch = new Channel<ArrayBuffer | ArrayLike<number>>();
    ch.onmessage = (m) =>
      onFrame(m instanceof ArrayBuffer ? new Uint8Array(m) : ArrayBuffer.isView(m) ? new Uint8Array((m as ArrayBufferView).buffer) : Uint8Array.from(m as ArrayLike<number>));
    const token = await invoke<number>("watch_preview", { id, onFrame: ch });
    return () => void invoke("unwatch_preview", { token });
  },
  preview: (id: number) => invoke<ArrayBuffer>("preview", { id }),
  diskFree: (path: string) => invoke<number | null>("disk_free", { path }),
  openPanelWindow: (id: string, title: string, width: number, height: number) =>
    invoke<void>("open_panel_window", { id, title, width, height }),
  closePanelWindow: (id: string) => invoke<void>("close_panel_window", { id }),
  readImage: (path: string) => invoke<ArrayBuffer>("read_image", { path }),
  pickImage: async () => {
    const f = await open({ filters: [{ name: "Image", extensions: ["png", "jpg", "jpeg"] }] });
    return typeof f === "string" ? f : null;
  },
  pickFolder: async () => {
    const dir = await open({ directory: true });
    return typeof dir === "string" ? dir : null;
  },
  openFolder: (path: string) => openPath(path),
};
