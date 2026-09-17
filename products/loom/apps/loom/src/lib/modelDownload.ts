/** Native-validated, inert definition from the current .mine.toml snapshot. */
export interface ConfiguredModelDownload {
  url: string;
  file_name: string;
  sha256: string;
  expected_bytes: number | null;
  max_bytes: number;
}

export interface ModelDownloadCapture {
  readonly commandId: string;
  readonly url: string;
  readonly fileName: string;
  readonly sha256: string;
  readonly expectedBytes: number | null;
  readonly maxBytes: number;
}

/** Copy once at the explicit click. Later config edits cannot change an uncertain retry. */
export function captureConfiguredDownload(commandId: string, definition: ConfiguredModelDownload): ModelDownloadCapture {
  return Object.freeze({
    commandId, url: definition.url, fileName: definition.file_name,
    sha256: definition.sha256.toLowerCase(), expectedBytes: definition.expected_bytes,
    maxBytes: definition.max_bytes
  });
}

export function formatByteCount(bytes: number | null | undefined): string {
  if (bytes === null || bytes === undefined || !Number.isFinite(bytes) || bytes < 0) return 'unknown';
  const units = ['B', 'KiB', 'MiB', 'GiB', 'TiB'];
  let value = bytes;
  let unit = 0;
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024;
    unit += 1;
  }
  const digits = unit === 0 ? 0 : value >= 10 ? 1 : 2;
  return `${value.toFixed(digits)} ${units[unit]}`;
}

export function downloadProgressPercent(downloaded: number, total: number | null): number | null {
  if (total === null || total <= 0 || downloaded < 0) return null;
  return Math.max(0, Math.min(100, (downloaded / total) * 100));
}
