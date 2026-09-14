import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
import { expect, it } from 'vitest';
const source = readFileSync(new URL('../../public/appearance-bootstrap.js', import.meta.url), 'utf8');
it('uses system appearance before first paint without reading stored overrides', () => {
  for (const dark of [false, true]) {
    const root = { dataset: {} as Record<string, string>, style: {} as Record<string, string> };
    runInNewContext(source, {
      document: { documentElement: root, querySelector: () => null },
      window: { get localStorage() { throw new Error('Appearance must not read persistence'); }, matchMedia: () => ({ matches: dark }) }
    });
    expect(root.dataset.theme).toBe(dark ? 'dark' : 'light');
    expect(root.style.colorScheme).toBe(root.dataset.theme);
  }
});
