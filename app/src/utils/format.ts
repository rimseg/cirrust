export function formatSize(bytes: number): string {
  if (bytes <= 0) return "—";
  const units = ["B", "KB", "MB", "GB", "TB"];
  const i = Math.min(units.length - 1, Math.floor(Math.log(bytes) / Math.log(1024)));
  const value = bytes / Math.pow(1024, i);
  return `${value.toFixed(i === 0 ? 0 : 1)} ${units[i]}`;
}

export function formatDate(iso: string | null): string {
  if (!iso) return "";
  const d = new Date(iso);
  if (Number.isNaN(d.getTime())) return "";
  return d.toLocaleString(undefined, {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function formatSpeed(bytesPerSec: number): string {
  if (bytesPerSec <= 0) return "0 B/s";
  return `${formatSize(bytesPerSec)}/s`;
}

export function basename(path: string): string {
  return path.replace(/\/+$/, "").split("/").pop() || path;
}

/** Parent directory of `path` ("" when it has none). */
export function dirname(path: string): string {
  const trimmed = path.replace(/\/+$/, "");
  const i = trimmed.lastIndexOf("/");
  return i > 0 ? trimmed.slice(0, i) : "";
}

export function relativeTime(iso: string): string {
  const d = new Date(iso).getTime();
  if (Number.isNaN(d)) return "";
  const secs = Math.round((Date.now() - d) / 1000);
  if (secs < 60) return "just now";
  if (secs < 3600) return `${Math.floor(secs / 60)}m ago`;
  if (secs < 86400) return `${Math.floor(secs / 3600)}h ago`;
  return `${Math.floor(secs / 86400)}d ago`;
}

const AUDIO_EXT = ["mp3", "flac", "ogg", "oga", "wav", "m4a", "aac", "opus", "wma"];

export function isAudio(name: string, contentType: string | null): boolean {
  if (contentType?.startsWith("audio/")) return true;
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  return AUDIO_EXT.includes(ext);
}

const IMAGE_EXT = ["png", "jpg", "jpeg", "gif", "webp", "bmp", "svg", "avif", "ico"];
const TEXT_EXT = [
  "txt", "md", "markdown", "json", "xml", "yaml", "yml", "csv", "tsv", "log",
  "js", "ts", "tsx", "jsx", "rs", "py", "sh", "bash", "c", "cpp", "h", "hpp",
  "java", "go", "rb", "php", "html", "htm", "css", "scss", "toml", "ini",
  "conf", "cfg", "sql", "vue", "kt", "swift", "lua",
];

const VIDEO_EXT = ["mp4", "webm", "mkv", "mov", "avi", "m4v", "ogv"];

export type PreviewKind = "image" | "video" | "text" | "pdf" | "none";

export function previewKind(name: string, contentType: string | null): PreviewKind {
  const ext = name.split(".").pop()?.toLowerCase() ?? "";
  if (contentType?.startsWith("image/") || IMAGE_EXT.includes(ext)) return "image";
  if (contentType?.startsWith("video/") || VIDEO_EXT.includes(ext)) return "video";
  if (ext === "pdf" || contentType === "application/pdf") return "pdf";
  if (contentType?.startsWith("text/") || TEXT_EXT.includes(ext)) return "text";
  return "none";
}

export function isImage(name: string, contentType: string | null): boolean {
  return previewKind(name, contentType) === "image";
}
