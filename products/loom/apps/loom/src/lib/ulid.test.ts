import { describe, expect, it } from 'vitest';
import { ulidFromParts } from './ulid';

describe('ULID command identity', () => {
  it('encodes the canonical zero and maximum values', () => {
    expect(ulidFromParts(0, new Uint8Array(10))).toBe('00000000000000000000000000');
    expect(ulidFromParts(0xffff_ffff_ffff, new Uint8Array(10).fill(0xff))).toBe(
      '7ZZZZZZZZZZZZZZZZZZZZZZZZZ'
    );
  });

  it('rejects values that cannot be a Rust ULID', () => {
    expect(() => ulidFromParts(-1, new Uint8Array(10))).toThrow(/48-bit/);
    expect(() => ulidFromParts(1, new Uint8Array(9))).toThrow(/10 bytes/);
  });
});
