import assert from 'node:assert/strict';
import { describe, it } from 'vitest';
import { observeInlineGhost, inlineGhostPreview, visualGhostInsertionIsVisible } from './inlineGhostObservation';

// Geometry inputs are explicit unit fixtures, not browser/native acceptance.
function fixture() {
  let text = ' world';
  let key = 'stream:run-1:7';
  const styles = new Map<object, { display: string; visibility: string; opacity: string; direction: string }>();
  const visible = { display: 'inline', visibility: 'visible', opacity: '1', direction: 'ltr' };
  const ownerDocument = { defaultView: { getComputedStyle: (node: object) => styles.get(node) ?? visible } };
  const outer = { parentElement: null, ownerDocument };
  const root = { parentElement: outer, ownerDocument };
  const widget = {
    isConnected: true, hidden: false, parentElement: root, ownerDocument,
    get textContent() { return text; },
    getAttribute: () => key,
    getBoundingClientRect: () => ({ left: 40, top: 40, right: 120, bottom: 60 })
  };
  const caret = { left: 40, top: 40, right: 40, bottom: 60 };
  const clip = { left: 0, top: 0, right: 500, bottom: 500 };
  const observe = () => observeInlineGhost(widget as unknown as HTMLElement, 'stream:run-1:7', ' world', caret, clip);
  return { widget, outer, styles, visible, observe, setText: (next: string) => text = next, setKey: (next: string) => key = next };
}

describe('inline ghost observation', () => {
  it('uses the production one-word projection rather than the complete candidate', () => {
    assert.equal(inlineGhostPreview(' world again'), ' world');
    assert.equal(inlineGhostPreview('world again'), 'world');
    assert.equal(inlineGhostPreview(' 雨 later'), ' 雨');
    assert.equal(inlineGhostPreview(' world '), ' world');
    assert.equal(inlineGhostPreview('   '), '');
  });

  it('permits a zero-width caret but not a zero-width ghost in both writing directions', () => {
    const clip = { left: 0, top: 0, right: 500, bottom: 500 };
    assert.equal(visualGhostInsertionIsVisible({ left: 80, top: 10, right: 80, bottom: 30 },
      { left: 20, top: 10, right: 80, bottom: 30 }, clip, 'rtl'), true);
    assert.equal(visualGhostInsertionIsVisible({ left: 80, top: 10, right: 80, bottom: 30 },
      { left: 80, top: 10, right: 80, bottom: 30 }, clip, 'rtl'), false);
  });
  it('reports the actual one-word DOM text, not the full candidate', () => {
    assert.deepEqual(fixture().observe(), { presentationKey: 'stream:run-1:7', text: ' world', utf8Bytes: 6 });
  });

  it('rejects empty or substituted glyphs even when the key and geometry still match', () => {
    const state = fixture();
    for (const text of ['', ' ', ' forged', ' world continues']) {
      state.setText(text);
      assert.equal(state.observe(), null);
    }
  });

  it('rejects stale identity, disconnected nodes and hidden nodes', () => {
    const state = fixture();
    state.setKey('stale');
    assert.equal(state.observe(), null);
    state.setKey('stream:run-1:7');
    state.widget.isConnected = false;
    assert.equal(state.observe(), null);
    state.widget.isConnected = true;
    state.widget.hidden = true;
    assert.equal(state.observe(), null);
  });

  it('checks ancestors outside the editor instead of certifying a hidden pane', () => {
    const state = fixture();
    for (const hidden of [{ display: 'none' }, { visibility: 'hidden' }, { visibility: 'collapse' }, { opacity: '0' }]) {
      state.styles.set(state.outer, { ...state.visible, ...hidden });
      assert.equal(state.observe(), null);
    }
  });

  it('rejects offscreen, zero-area and nonfinite glyph geometry', () => {
    const state = fixture();
    for (const rect of [
      { left: 600, top: 40, right: 680, bottom: 60 },
      { left: 40, top: 40, right: 40, bottom: 60 },
      { left: 40, top: 40, right: 120, bottom: 40 },
      { left: NaN, top: 40, right: 120, bottom: 60 }
    ]) {
      state.widget.getBoundingClientRect = () => rect;
      assert.equal(state.observe(), null);
    }
  });

  it('counts UTF-8 bytes, not UTF-16 code units', () => {
    const state = fixture();
    state.setText(' 雨');
    assert.deepEqual(observeInlineGhost(state.widget as unknown as HTMLElement, 'stream:run-1:7', ' 雨',
      { left: 40, top: 40, right: 40, bottom: 60 }, { left: 0, top: 0, right: 500, bottom: 500 }), { presentationKey: 'stream:run-1:7', text: ' 雨', utf8Bytes: 4 });
  });
});
