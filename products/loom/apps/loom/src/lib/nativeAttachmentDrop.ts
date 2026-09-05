export type DropScope = 'context' | 'inline';
export type DropPoint = { x: number; y: number };
export type DropSurface = { scope: DropScope; bounds: Pick<DOMRect, 'left' | 'right' | 'top' | 'bottom'> };

// Wry 0.55.1 WKWebView emits AppKit logical points, despite the Tauri
// PhysicalPosition wrapper. Windows/Linux emit physical pixels.
export function nativeDropPoint(position: DropPoint, platform: string, scale: number): DropPoint {
  const divisor = /Mac/i.test(platform) ? 1 : (scale > 0 ? scale : 1);
  return { x: position.x / divisor, y: position.y / divisor };
}

export function nativeDropScope(point: DropPoint, surfaces: readonly DropSurface[]): DropScope | null {
  // Context owns its entire surface, including blank content and child controls.
  for (const scope of ['context', 'inline'] as const) {
    if (surfaces.some(surface => surface.scope === scope &&
      point.x >= surface.bounds.left && point.x < surface.bounds.right &&
      point.y >= surface.bounds.top && point.y < surface.bounds.bottom)) return scope;
  }
  return null;
}
