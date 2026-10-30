export type SplitLayout = "new_folder" | "same_folder";
export type Encoder = "video_toolbox" | "nvenc" | "qsv" | "amf" | "media_foundation" | "x264";

export type Deinterlace = "off" | "on";

export interface ChannelSetting {
  enabled: boolean;
  /** null = synthetic test pattern */
  device: string | null;
  size: string | null;
  fps: number;
  deinterlace: Deinterlace;
}

export type ItemKind = "text" | "image";

/** One object on the frame. Positions/sizes are fractions of the frame (0..1). */
export interface OverlayItem {
  id: string;
  enabled: boolean;
  kind: ItemKind;
  x: number;
  y: number;
  text: string;
  /** font height as a fraction of frame height */
  size: number;
  color: string;
  bold: boolean;
  bg_color: string;
  bg_opacity: number;
  image: string | null;
  /** image width as a fraction of frame width */
  width: number;
  opacity: number;
}

export type DataSource = { kind: "none" } | { kind: "simulator" } | { kind: "udp_json"; port: number };

export type OverlayMode = "layer" | "burn";

export interface OverlayConfig {
  enabled: boolean;
  /** layer = editable while recording (more CPU/RAM); burn = fixed once recording starts */
  mode: OverlayMode;
  font_file: string | null;
  items: OverlayItem[];
  source: DataSource;
}

export type LowSpaceAction = "warn" | "delete_oldest" | "stop";

export interface StorageConfig {
  min_free_gb: number;
  on_low: LowSpaceAction;
  max_age_days: number;
}

export interface Settings {
  root: string;
  segment_minutes: number;
  split_layout: SplitLayout;
  encoder: Encoder | null;
  bitrate: string;
  preview: boolean;
  preview_width: number;
  overlay: OverlayConfig;
  storage: StorageConfig;
  channels: ChannelSetting[];
}

export type ChannelState = "starting" | "recording" | "restarting" | "no_signal" | "stopped";

export interface ChannelStatus {
  id: number;
  /** frame rate actually requested from the device */
  fps: number;
  state: ChannelState;
  frames: number;
  dropped: number;
  speed: number;
  segments: number;
  restarts: number;
  cpu_percent: number;
  rss_mb: number;
  last_file: string | null;
  last_error: string | null;
}

export interface Status {
  /** false = live monitor only (camera visible, nothing recorded) */
  recording: boolean;
  layer_active: boolean;
  started_ms: number;
  encoder: Encoder;
  warnings: string[];
  values: Record<string, string>;
  channels: ChannelStatus[];
}

export interface VideoDevice {
  name: string;
}

export interface Mode {
  w: number;
  h: number;
  fps: number[];
}
