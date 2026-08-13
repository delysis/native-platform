export const MAX_MODEL_DOWNLOAD_GIB = 1024;
export const DEFAULT_MODEL_DOWNLOAD_LIMIT_GIB = 64;
export const MAX_MODEL_DOWNLOAD_URL_BYTES = 16 * 1024;
export const MAX_MODEL_FILE_NAME_BYTES = 240;

export interface VerifiedDownloadForm {
  url: string;
  fileName: string;
  sha256: string;
  expectedBytes: number | null;
  maxBytes: number;
}

export interface DownloadFormInput {
  url: string;
  fileName: string;
  sha256: string;
  expectedBytes: string;
  maximumGiB: string;
}

export function deriveGgufFileName(value: string): string {
  let url: URL;
  try {
    url = new URL(value.trim());
  } catch {
    return '';
  }
  if (url.protocol !== 'https:' || url.username || url.password) return '';
  const encoded = url.pathname.split('/').filter(Boolean).at(-1) ?? '';
  let fileName: string;
  try {
    fileName = decodeURIComponent(encoded);
  } catch {
    return '';
  }
  return fileName.toLocaleLowerCase('en-US').endsWith('.gguf') ? fileName : '';
}

export function validateVerifiedDownload(input: DownloadFormInput): VerifiedDownloadForm {
  const urlValue = input.url.trim();
  let url: URL;
  try {
    url = new URL(urlValue);
  } catch {
    throw new Error('Enter a complete HTTPS model URL.');
  }
  if (url.protocol !== 'https:') throw new Error('Model downloads require HTTPS.');
  if (url.username || url.password) throw new Error('Do not put credentials in the model URL.');
  const normalizedUrl = url.toString();
  if (new TextEncoder().encode(normalizedUrl).length > MAX_MODEL_DOWNLOAD_URL_BYTES) {
    throw new Error('The model URL is too long.');
  }

  const fileName = input.fileName.trim();
  if (!fileName.toLocaleLowerCase('en-US').endsWith('.gguf')) {
    throw new Error('The local model name must end in .gguf.');
  }
  if (fileName.length === '.gguf'.length) {
    throw new Error('Choose a local model name before the .gguf extension.');
  }
  if (!/^[^<>:"/\\|?*\u0000-\u001f]+$/u.test(fileName) || /[. ]$/u.test(fileName)) {
    throw new Error('Choose one portable file name without folders or reserved characters.');
  }
  if (new TextEncoder().encode(fileName).length > MAX_MODEL_FILE_NAME_BYTES) {
    throw new Error(`The local model name cannot exceed ${MAX_MODEL_FILE_NAME_BYTES} UTF-8 bytes.`);
  }
  const firstStem = fileName.slice(0, -'.gguf'.length).split('.')[0]?.toLocaleUpperCase('en-US');
  if (
    firstStem &&
    (/^(CON|PRN|AUX|NUL)$/u.test(firstStem) || /^(COM|LPT)[1-9]$/u.test(firstStem))
  ) {
    throw new Error('Choose a model name that is portable across macOS, Windows, and Linux.');
  }

  const sha256 = input.sha256.trim().toLocaleLowerCase('en-US');
  if (!/^[0-9a-f]{64}$/u.test(sha256)) {
    throw new Error('Paste the publisher’s complete 64-character SHA-256 checksum.');
  }

  const maximumGiB = parsePositiveNumber(input.maximumGiB, 'maximum download size');
  if (maximumGiB > MAX_MODEL_DOWNLOAD_GIB) {
    throw new Error(`The maximum download size cannot exceed ${MAX_MODEL_DOWNLOAD_GIB} GiB.`);
  }
  const maxBytes = gibToBytes(maximumGiB);
  const expectedBytes = input.expectedBytes.trim()
    ? parsePositiveInteger(input.expectedBytes, 'expected byte count')
    : null;
  if (expectedBytes !== null && expectedBytes > maxBytes) {
    throw new Error('The expected byte count is larger than the maximum download size.');
  }

  return { url: normalizedUrl, fileName, sha256, expectedBytes, maxBytes };
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

function parsePositiveInteger(value: string, label: string): number {
  const trimmed = value.trim();
  if (!/^\d+$/u.test(trimmed)) throw new Error(`Enter the ${label} as a positive whole number.`);
  const parsed = Number(trimmed);
  if (!Number.isSafeInteger(parsed) || parsed <= 0) {
    throw new Error(`Enter the ${label} as a positive whole number.`);
  }
  return parsed;
}

function parsePositiveNumber(value: string, label: string): number {
  const parsed = Number(value.trim());
  if (!Number.isFinite(parsed) || parsed <= 0) throw new Error(`Enter a positive ${label}.`);
  return parsed;
}

function gibToBytes(gibibytes: number): number {
  const bytes = Math.floor(gibibytes * 1024 ** 3);
  if (!Number.isSafeInteger(bytes) || bytes <= 0) throw new Error('The maximum download size is not representable safely.');
  return bytes;
}
