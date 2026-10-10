import { expect, it } from 'vitest';
import { togglePaneVisibility, visiblePanes, type PanePosition } from './paneLayout';

it('keeps writing visible alone, expands chat, then restores writing when chat closes', () => {
  let hidden = togglePaneVisibility('main', [], new Set());
  expect(visiblePanes([], hidden)).toEqual({ main: true, right: false, bottom: false });
  hidden = togglePaneVisibility('main', ['right'], hidden);
  expect(visiblePanes(['right'], hidden)).toEqual({ main: false, right: true, bottom: false });
  hidden = togglePaneVisibility('right', ['right'], hidden);
  expect(visiblePanes(['right'], hidden)).toEqual({ main: true, right: false, bottom: false });
  hidden = togglePaneVisibility('right', ['right'], hidden);
  expect(visiblePanes(['right'], hidden)).toEqual({ main: true, right: true, bottom: false });
});

it('cannot produce an empty layout through toggles or removal of configured panes', () => {
  const positions: PanePosition[] = ['main', 'right', 'bottom'];
  for (let configured = 0; configured < 8; configured++) {
    const equipped = positions.filter((_, i) => configured & (1 << i));
    for (let collapsed = 0; collapsed < 8; collapsed++) {
      const hidden = new Set(positions.filter((_, i) => collapsed & (1 << i)));
      expect(Object.values(visiblePanes(equipped, hidden)).some(Boolean)).toBe(true);
      for (const position of positions) {
        const next = togglePaneVisibility(position, equipped, hidden);
        expect(Object.values(visiblePanes(equipped, next)).some(Boolean)).toBe(true);
        expect(visiblePanes([], next).main).toBe(true);
      }
    }
  }
});
