const CROCKFORD = '0123456789ABCDEFGHJKMNPQRSTVWXYZ';
const MAX_TIMESTAMP = 0xffff_ffff_ffff;

function encode(value: bigint, length: number): string {
  const characters = new Array<string>(length);
  for (let index = length - 1; index >= 0; index -= 1) {
    characters[index] = CROCKFORD[Number(value & 31n)];
    value >>= 5n;
  }
  if (value !== 0n) throw new RangeError('value does not fit in the requested ULID field');
  return characters.join('');
}

export function ulidFromParts(timestampMs: number, randomness: Uint8Array): string {
  if (!Number.isSafeInteger(timestampMs) || timestampMs < 0 || timestampMs > MAX_TIMESTAMP) {
    throw new RangeError('ULID timestamp must be an unsigned 48-bit integer');
  }
  if (randomness.length !== 10) throw new RangeError('ULID randomness must contain 10 bytes');

  let random = 0n;
  for (const byte of randomness) random = (random << 8n) | BigInt(byte);
  return encode(BigInt(timestampMs), 10) + encode(random, 16);
}

export function newUlid(): string {
  return ulidFromParts(Date.now(), crypto.getRandomValues(new Uint8Array(10)));
}
