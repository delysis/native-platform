import { describe, expect, it } from 'vitest';
import {
  deriveGgufFileName,
  downloadProgressPercent,
  formatByteCount,
  validateVerifiedDownload,
} from './modelDownload';

describe('verified model download helpers', () => {
  it('derives a decoded GGUF name only from credential-free HTTPS URLs', () => {
    expect(deriveGgufFileName('https://models.example/Gemma%204.Q8_0.gguf?download=1')).toBe(
      'Gemma 4.Q8_0.gguf',
    );
    expect(deriveGgufFileName('http://models.example/model.gguf')).toBe('');
    expect(deriveGgufFileName('https://token@models.example/model.gguf')).toBe('');
    expect(deriveGgufFileName('https://models.example/model.bin')).toBe('');
  });

  it('requires an exact checksum and bounded sizes', () => {
    const request = validateVerifiedDownload({
      url: 'https://models.example/writer.gguf',
      fileName: 'writer.gguf',
      sha256: 'AB'.repeat(32),
      expectedBytes: '4954576032',
      maximumGiB: '8',
    });
    expect(request.sha256).toBe('ab'.repeat(32));
    expect(request.expectedBytes).toBe(4_954_576_032);
    expect(request.maxBytes).toBe(8 * 1024 ** 3);
  });

  it('rejects unsafe names and impossible bounds', () => {
    const base = {
      url: 'https://models.example/writer.gguf',
      fileName: '../writer.gguf',
      sha256: 'ab'.repeat(32),
      expectedBytes: '',
      maximumGiB: '8',
    };
    expect(() => validateVerifiedDownload(base)).toThrow(/portable file name/u);
    expect(() =>
      validateVerifiedDownload({ ...base, fileName: 'writer.gguf', expectedBytes: '9000000000' }),
    ).toThrow(/larger than the maximum/u);
    expect(() =>
      validateVerifiedDownload({ ...base, fileName: 'CON.gguf' }),
    ).toThrow(/portable across/u);
    expect(() =>
      validateVerifiedDownload({ ...base, fileName: '.gguf' }),
    ).toThrow(/model name/u);
    expect(() =>
      validateVerifiedDownload({ ...base, fileName: `${'é'.repeat(119)}.gguf` }),
    ).toThrow(/UTF-8 bytes/u);
  });

  it('formats byte evidence and clamps progress', () => {
    expect(formatByteCount(4_954_576_032)).toBe('4.61 GiB');
    expect(formatByteCount(null)).toBe('unknown');
    expect(downloadProgressPercent(50, 200)).toBe(25);
    expect(downloadProgressPercent(250, 200)).toBe(100);
    expect(downloadProgressPercent(0, null)).toBeNull();
  });

  it('turns fractional GiB ceilings into conservative whole-byte bounds', () => {
    const request = validateVerifiedDownload({
      url: 'https://models.example/writer.gguf',
      fileName: 'writer.gguf',
      sha256: 'ab'.repeat(32),
      expectedBytes: '',
      maximumGiB: '0.001',
    });
    expect(request.maxBytes).toBe(Math.floor(0.001 * 1024 ** 3));
  });
});
