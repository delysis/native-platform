import { describe, expect, it } from 'vitest';
import {
  MAX_IMAGE_ATTACHMENT_BYTES,
  MAX_IMAGE_ATTACHMENTS_PER_TRANSFER,
  MAX_IMAGE_ATTACHMENT_TRANSFER_BYTES,
  acceptedImageFile,
  attachmentMarkdown,
  imageAttachmentTransferError,
  imageAttachmentErrorMessage,
  projectAssetProtocolToken,
  transferContainsEphemeralImage,
  transferMayContainImageFile,
  type ImageAttachmentReceipt
} from './attachments';

const projectId = '01J00000000000000000000000';
const sessionId = '01J00000000000000000000001';

const receipt: ImageAttachmentReceipt = {
  relative_path: 'assets/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef.png',
  markdown_path: '../assets/0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef.png',
  media_type: 'image/png',
  byte_count: 4,
  sha256: '0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef'
};

describe('image attachments', () => {
  it('admits only bounded browser-renderable raster images', () => {
    expect(acceptedImageFile({ name: 'page.png', size: 4, type: 'image/png' })).toBeNull();
    expect(acceptedImageFile({ name: 'page.svg', size: 4, type: 'image/svg+xml' })).toContain('not a supported');
    expect(acceptedImageFile({ name: 'empty.png', size: 0, type: 'image/png' })).toContain('empty');
    expect(acceptedImageFile({ name: 'huge.png', size: MAX_IMAGE_ATTACHMENT_BYTES + 1, type: 'image/png' })).toContain('16 MB');
  });

  it('creates portable Markdown without trusting the source filename as syntax', () => {
    expect(attachmentMarkdown(receipt, 'Sketch [final]\\.png')).toBe(
      `![Sketch final](${receipt.markdown_path})`
    );
    expect(attachmentMarkdown(receipt, 'Map.png', 'manuscript/chapters/one.md')).toBe(
      `![Map](../../${receipt.relative_path})`
    );
  });

  it('builds only exact project/session-scoped content-addressed asset tokens', () => {
    const expected = `v1-${projectId}-${sessionId}-${receipt.sha256}.png`;
    expect(projectAssetProtocolToken(projectId, sessionId, receipt.markdown_path)).toBe(expected);
    expect(projectAssetProtocolToken(projectId, sessionId, `../../${receipt.relative_path}`))
      .toBe(expected);
    expect(projectAssetProtocolToken(projectId, sessionId, '../outside/passwords.png')).toBeNull();
    expect(projectAssetProtocolToken(projectId, sessionId, '../assets/not-a-digest.png')).toBeNull();
    expect(projectAssetProtocolToken(projectId.toLowerCase(), sessionId, receipt.markdown_path))
      .toBeNull();
  });

  it('bounds one multi-image transfer before allocating image payloads', () => {
    expect(imageAttachmentTransferError([{ size: MAX_IMAGE_ATTACHMENT_TRANSFER_BYTES }]))
      .toBeNull();
    expect(imageAttachmentTransferError([
      { size: MAX_IMAGE_ATTACHMENT_TRANSFER_BYTES },
      { size: 1 }
    ])).toContain('64 MB');
    expect(imageAttachmentTransferError(
      Array.from({ length: MAX_IMAGE_ATTACHMENTS_PER_TRANSFER + 1 }, () => ({ size: 1 }))
    )).toContain(`${MAX_IMAGE_ATTACHMENTS_PER_TRANSFER}`);
  });

  it('recognizes browser-owned image URLs that must never enter persisted Markdown', () => {
    const transfer = {
      getData: (kind: string) => kind === 'text/html'
        ? '<p><img alt="paste" src="data:image/png;base64,AAAA"></p>'
        : ''
    } as DataTransfer;
    expect(transferContainsEphemeralImage(transfer)).toBe(true);
    expect(transferContainsEphemeralImage({
      getData: () => '<p><img src="https://example.com/image.png"></p>'
    } as unknown as DataTransfer)).toBe(false);
  });

  it('recognizes protected drag metadata before drop exposes file bytes', () => {
    expect(transferMayContainImageFile({
      files: [] as unknown as FileList,
      items: [{ kind: 'file', type: 'image/png' }] as unknown as DataTransferItemList,
      types: ['Files']
    } as unknown as DataTransfer)).toBe(true);
  });

  it('preserves explicit attachment failures and supplies a bounded fallback', () => {
    expect(imageAttachmentErrorMessage(new Error('Disk full.'))).toBe('Disk full.');
    expect(imageAttachmentErrorMessage('opaque rejection')).toBe('Loom could not attach that image.');
  });
});
