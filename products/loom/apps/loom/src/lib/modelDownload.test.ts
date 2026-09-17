import { describe, expect, it } from 'vitest';
import { captureConfiguredDownload, downloadProgressPercent, formatByteCount } from './modelDownload';

describe('configured model download commands', () => {
  it('freezes every request field before later settings edits or uncertain retries', () => {
    const definition = {
      url: 'https://models.example/writer.gguf?revision=one',
      file_name: 'writer.gguf', sha256: 'AB'.repeat(32),
      expected_bytes: 4_954_576_032, max_bytes: 8_589_934_592
    };
    const request = captureConfiguredDownload('one-command', definition);
    definition.url = 'https://models.example/replaced.gguf';
    definition.file_name = 'replaced.gguf';
    definition.sha256 = 'cd'.repeat(32);
    definition.expected_bytes = 10; definition.max_bytes = 20;
    expect(request).toEqual({
      commandId: 'one-command', url: 'https://models.example/writer.gguf?revision=one',
      fileName: 'writer.gguf', sha256: 'ab'.repeat(32),
      expectedBytes: 4_954_576_032, maxBytes: 8_589_934_592
    });
    expect(Object.isFrozen(request)).toBe(true);
  });

  it('retains an omitted exact size and a byte-exact ceiling', () => {
    expect(captureConfiguredDownload('id', {
      url: 'https://models.example/projector.gguf', file_name: 'projector.gguf',
      sha256: 'ab'.repeat(32), expected_bytes: null, max_bytes: 1_073_741
    })).toMatchObject({ expectedBytes: null, maxBytes: 1_073_741 });
  });

  it('formats byte evidence and clamps progress', () => {
    expect(formatByteCount(4_954_576_032)).toBe('4.61 GiB');
    expect(formatByteCount(null)).toBe('unknown');
    expect(downloadProgressPercent(50, 200)).toBe(25);
    expect(downloadProgressPercent(250, 200)).toBe(100);
    expect(downloadProgressPercent(0, null)).toBeNull();
  });
});
