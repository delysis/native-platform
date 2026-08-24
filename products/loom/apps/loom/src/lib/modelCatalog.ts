import type {
  CuratedModelCatalogEntry,
  CuratedModelCatalogSnapshot,
  ModelCapabilitySummary
} from './types';

const SHA256_PATTERN = /^[0-9a-f]{64}$/u;
const REVISION_PATTERN = /^[0-9a-f]{40}$/u;
const CATALOG_ID_PATTERN = /^[a-z0-9]+(?:[._-][a-z0-9]+)*$/u;

function isPositiveSafeInteger(value: number): boolean {
  return Number.isSafeInteger(value) && value > 0;
}

function expectedDownloadUrl(entry: CuratedModelCatalogEntry): string {
  return `https://huggingface.co/${entry.repository}/resolve/${entry.revision}/${entry.artifact_name}?download=true`;
}

function isVisionAdapter(model: ModelCapabilitySummary): boolean {
  return model.projector_present === true ||
    model.media_kinds.length > 0 ||
    /(^|[-_.])(mmproj|projector)([-_.]|$)/iu.test(model.display_name) ||
    /(^|[/\\-_.])(mmproj|projector)([/\\-_.]|$)/iu.test(model.model_path);
}

function validateEntry(entry: CuratedModelCatalogEntry): void {
  if (
    !CATALOG_ID_PATTERN.test(entry.catalog_id) ||
    !entry.display_name.trim() ||
    !entry.publisher.trim() ||
    !/^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+$/u.test(entry.repository) ||
    !REVISION_PATTERN.test(entry.revision) ||
    !/^[^/\\]+\.gguf$/iu.test(entry.artifact_name) ||
    entry.download_url !== expectedDownloadUrl(entry) ||
    !SHA256_PATTERN.test(entry.expected_sha256) ||
    !isPositiveSafeInteger(entry.expected_bytes) ||
    entry.max_bytes !== entry.expected_bytes ||
    !isPositiveSafeInteger(entry.context_tokens) ||
    entry.license.spdx_id !== 'Apache-2.0' ||
    !entry.license.name.trim() ||
    !entry.license.url.startsWith('https://') ||
    entry.memory_fit.weight_bytes !== entry.expected_bytes ||
    !isPositiveSafeInteger(entry.memory_fit.recommended_system_memory_bytes) ||
    entry.memory_fit.recommended_system_memory_bytes < entry.memory_fit.weight_bytes ||
    !entry.memory_fit.description.trim() ||
    entry.compatibility.local_only !== true ||
    entry.compatibility.hosted_fallback !== false ||
    entry.compatibility.prompt_mode !== 'raw_completion' ||
    entry.compatibility.native_inspection_required !== true ||
    entry.compatibility.legacy_local_file_name !== entry.artifact_name ||
    entry.compatibility.legacy_local_file_bytes !== entry.expected_bytes
  ) {
    throw new Error('The desktop returned an invalid curated model catalog entry.');
  }
}

/**
 * Validates the native embedded catalog before any entry can reach the
 * verified downloader. The catalog is bounded and contains data only; it
 * grants neither network nor model-load authority by being displayed.
 */
export function validateCuratedModelCatalog(
  snapshot: CuratedModelCatalogSnapshot
): CuratedModelCatalogEntry[] {
  if (
    snapshot.schema_version !== 1 ||
    snapshot.entries.length !== 1
  ) {
    throw new Error('The desktop returned an unsupported curated model catalog.');
  }
  for (const entry of snapshot.entries) {
    validateEntry(entry);
  }
  return snapshot.entries;
}

/**
 * Legacy discovery has only GGUF header, file-name, and byte-length facts.
 * This exact match may prioritize an already-local artifact, but it never
 * establishes the catalog checksum or bypasses native model inspection.
 */
export function legacyLocalCatalogMatch(
  entry: CuratedModelCatalogEntry,
  model: ModelCapabilitySummary
): boolean {
  return model.local &&
    model.header_verified &&
    !model.loaded &&
    !isVisionAdapter(model) &&
    model.display_name.toLocaleLowerCase('en-US') ===
      entry.compatibility.legacy_local_file_name.toLocaleLowerCase('en-US') &&
    model.file_bytes === entry.compatibility.legacy_local_file_bytes;
}

/**
 * The renderer independently checks the exact identity returned by native
 * catalog admission before it selects the model for the writing session.
 */
export function isVerifiedCatalogWriter(
  entry: CuratedModelCatalogEntry,
  model: ModelCapabilitySummary
): boolean {
  return model.local &&
    model.loaded &&
    model.header_verified &&
    model.model_sha256 === entry.expected_sha256 &&
    model.file_bytes === entry.expected_bytes &&
    model.completion &&
    model.output_tokens &&
    !isVisionAdapter(model);
}

export function catalogDownloadRequest(entry: CuratedModelCatalogEntry): {
  url: string;
  fileName: string;
  sha256: string;
  expectedBytes: number;
  maxBytes: number;
} {
  validateEntry(entry);
  return {
    url: entry.download_url,
    fileName: entry.artifact_name,
    sha256: entry.expected_sha256,
    expectedBytes: entry.expected_bytes,
    maxBytes: entry.max_bytes
  };
}
