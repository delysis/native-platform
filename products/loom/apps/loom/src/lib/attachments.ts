export const MAX_IMAGE_ATTACHMENT_BYTES = 16 * 1024 * 1024;
export const MAX_IMAGE_ATTACHMENTS_PER_TRANSFER = 16;
export const MAX_IMAGE_ATTACHMENT_TRANSFER_BYTES = 64 * 1024 * 1024;
export const UNREADABLE_TRANSFER_IMAGE_ERROR =
  'Loom could not read image bytes from that paste. Copy or drag the original image file instead.';
export const UNVERIFIED_DROP_FILE_ERROR =
  'Loom could not verify that dropped file as a supported PNG, JPEG, GIF, or WebP image.';
export const STALE_IMAGE_ATTACHMENT_ERROR =
  'The image was stored in project assets, but the manuscript changed before it could be inserted.';
export const IMAGE_ATTACHMENT_FAILED_ERROR = 'Loom could not attach that image.';

export function imageAttachmentErrorMessage(error: unknown): string {
  return error instanceof Error && error.message.trim()
    ? error.message
    : IMAGE_ATTACHMENT_FAILED_ERROR;
}

const acceptedImageTypes = new Set([
  'image/png',
  'image/jpeg',
  'image/gif',
  'image/webp'
]);

export interface ImageAttachmentReceipt {
  relative_path: string;
  markdown_path: string;
  media_type: string;
  byte_count: number;
  sha256: string;
}

export interface EncodedImageAttachment {
  originalName: string;
  mediaType: string;
  base64: string;
}

export function acceptedImageFile(file: Pick<File, 'name' | 'size' | 'type'>): string | null {
  if (!acceptedImageTypes.has(file.type.toLowerCase())) {
    return `${file.name || 'That file'} is not a supported PNG, JPEG, GIF, or WebP image.`;
  }
  if (file.size === 0) return `${file.name || 'That image'} is empty.`;
  if (file.size > MAX_IMAGE_ATTACHMENT_BYTES) {
    return `${file.name || 'That image'} is larger than Loom's 16 MB attachment limit.`;
  }
  return null;
}

export function imageAttachmentTransferError(
  files: readonly Pick<File, 'size'>[]
): string | null {
  if (files.length > MAX_IMAGE_ATTACHMENTS_PER_TRANSFER) {
    return `Attach at most ${MAX_IMAGE_ATTACHMENTS_PER_TRANSFER} images at once.`;
  }
  let totalBytes = 0;
  for (const file of files) {
    if (!Number.isSafeInteger(file.size) || file.size < 0) {
      return 'Loom could not verify the size of every selected image.';
    }
    totalBytes += file.size;
    if (!Number.isSafeInteger(totalBytes) || totalBytes > MAX_IMAGE_ATTACHMENT_TRANSFER_BYTES) {
      return 'Attach no more than 64 MB of images at once.';
    }
  }
  return null;
}

export function imageFilesFromTransfer(transfer: DataTransfer | null): File[] {
  if (!transfer) return [];
  return Array.from(transfer.files).filter((file) => file.type.toLowerCase().startsWith('image/'));
}

export function transferMayContainImageFile(transfer: DataTransfer | null): boolean {
  if (!transfer) return false;
  if (imageFilesFromTransfer(transfer).length > 0) return true;
  if (Array.from(transfer.items).some(
    (item) => item.kind === 'file' && item.type.toLowerCase().startsWith('image/')
  )) return true;
  return Array.from(transfer.types).includes('Files');
}

/** Refuse browser-owned ephemeral image URLs even when no file bytes are exposed. */
export function transferContainsEphemeralImage(transfer: DataTransfer | null): boolean {
  if (!transfer) return false;
  const html = transfer.getData('text/html');
  return /<img\b[^>]*\bsrc\s*=\s*(?:["']\s*)?(?:data:image\/|blob:)/iu.test(html);
}

export async function encodeImageAttachment(file: File): Promise<EncodedImageAttachment> {
  const rejection = acceptedImageFile(file);
  if (rejection) throw new Error(rejection);
  const bytes = new Uint8Array(await file.arrayBuffer());
  let binary = '';
  const chunkSize = 0x8000;
  for (let offset = 0; offset < bytes.length; offset += chunkSize) {
    binary += String.fromCharCode(...bytes.subarray(offset, offset + chunkSize));
  }
  return {
    originalName: file.name,
    mediaType: file.type.toLowerCase(),
    base64: btoa(binary)
  };
}

export function attachmentMarkdown(
  receipt: ImageAttachmentReceipt,
  originalName: string,
  documentRelativePath = 'manuscript/Untitled.md'
): string {
  const alt = originalName
    .replace(/\.[^.]+$/u, '')
    .replace(/[\[\]\\]/gu, ' ')
    .replace(/\s+/gu, ' ')
    .trim();
  const components = documentRelativePath.split('/');
  const safeDocumentPath = components.length >= 2 &&
    components[0] === 'manuscript' &&
    components.every((component) => component.length > 0 && component !== '.' && component !== '..');
  const markdownPath = safeDocumentPath
    ? `${'../'.repeat(components.length - 1)}${receipt.relative_path}`
    : receipt.markdown_path;
  return `![${alt || 'Image'}](${markdownPath})`;
}

export function projectAssetProtocolToken(
  projectId: string,
  sessionId: string,
  markdownPath: string
): string | null {
  const match = /^(?:\.\.\/)+assets\/([a-f0-9]{64}\.(?:png|jpg|gif|webp))$/u.exec(markdownPath);
  const canonicalUlid = /^[0-9A-HJKMNP-TV-Z]{26}$/u;
  if (!match || !canonicalUlid.test(projectId) || !canonicalUlid.test(sessionId)) return null;
  return `v1-${projectId}-${sessionId}-${match[1]}`;
}
