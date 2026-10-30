export const pad = (n: number) => String(n).padStart(2, "0");

export function clock(totalSec: number): string {
  const s = Math.max(0, Math.floor(totalSec));
  return `${pad(Math.floor(s / 3600))}:${pad(Math.floor((s % 3600) / 60))}:${pad(s % 60)}`;
}

export function gb(bytes: number | null): string {
  return bytes == null ? "—" : `${(bytes / 1024 ** 3).toFixed(1)} GB`;
}

export const ENCODER_LABEL: Record<string, string> = {
  video_toolbox: "Apple VideoToolbox (hardware)",
  nvenc: "NVIDIA NVENC (hardware)",
  qsv: "Intel QuickSync (hardware)",
  amf: "AMD AMF (hardware)",
  media_foundation: "Windows Media Foundation (hardware or built-in software)",
  x264: "x264 (software)",
};
