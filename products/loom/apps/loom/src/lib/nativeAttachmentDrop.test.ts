import { describe, it, expect } from 'vitest';
import { nativeDropPoint, nativeDropScope } from './nativeAttachmentDrop';

describe('native file drop ownership', () => {
  const surfaces = [
    { scope: 'inline' as const, bounds: { left: 0, right: 900, top: 100, bottom: 1000 } },
    { scope: 'context' as const, bounds: { left: 200, right: 900, top: 100, bottom: 400 } }
  ];
  it('does not divide AppKit logical coordinates by Retina scale', () => {
    const point = nativeDropPoint({ x: 700, y: 390 }, 'MacIntel', 2);
    expect(point).toEqual({ x: 700, y: 390 });
    expect(nativeDropScope(point, surfaces)).toBe('context');
  });
  it('converts physical pixels on other platforms', () => {
    expect(nativeDropPoint({ x: 1400, y: 780 }, 'Win32', 2)).toEqual({ x: 700, y: 390 });
  });
  it('owns blank lower context space with no manuscript fallthrough', () => {
    expect(nativeDropScope({ x: 899, y: 399 }, surfaces)).toBe('context');
    expect(nativeDropScope({ x: 899, y: 400 }, surfaces)).toBe('inline');
    expect(nativeDropScope({ x: 999, y: 399 }, surfaces)).toBeNull();
  });
});
