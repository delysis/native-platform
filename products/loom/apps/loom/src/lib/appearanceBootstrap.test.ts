import { readFileSync } from 'node:fs';
import { runInNewContext } from 'node:vm';
import { describe, expect, it } from 'vitest';
import { APPEARANCE_PREFERENCE_KEY } from './appearance';

const bootstrapSource = readFileSync(
  new URL('../../public/appearance-bootstrap.js', import.meta.url),
  'utf8'
);

function runBootstrap(
  stored: string | null,
  systemDark: boolean,
  storageUnavailable = false
): { theme: string; colorScheme: string; themeColor: string } {
  const root = { dataset: {} as Record<string, string>, style: {} as Record<string, string> };
  const themeColor = { content: '#f4f0e8' };
  const localStorage = {
    getItem: (key: string): string | null => {
      expect(key).toBe(APPEARANCE_PREFERENCE_KEY);
      if (storageUnavailable) throw new Error('storage unavailable');
      return stored;
    }
  };
  runInNewContext(bootstrapSource, {
    document: {
      documentElement: root,
      querySelector: (selector: string) => selector === 'meta[name="theme-color"]'
        ? themeColor
        : null
    },
    window: {
      localStorage,
      matchMedia: (query: string) => {
        expect(query).toBe('(prefers-color-scheme: dark)');
        return { matches: systemDark };
      }
    }
  });
  return {
    theme: root.dataset.theme ?? '',
    colorScheme: root.style.colorScheme ?? '',
    themeColor: themeColor.content
  };
}

describe('pre-paint appearance bootstrap', () => {
  it('applies persisted and system-dark choices before the application module', () => {
    expect(runBootstrap('dark', false)).toEqual({
      theme: 'dark', colorScheme: 'dark', themeColor: '#222420'
    });
    expect(runBootstrap('light', true)).toEqual({
      theme: 'light', colorScheme: 'light', themeColor: '#f4f0e8'
    });
    expect(runBootstrap('system', true)).toEqual({
      theme: 'dark', colorScheme: 'dark', themeColor: '#222420'
    });
    expect(runBootstrap(null, true, true)).toEqual({
      theme: 'dark', colorScheme: 'dark', themeColor: '#222420'
    });
  });

  it('loads the same-origin bootstrap synchronously in the head', () => {
    const index = readFileSync(new URL('../../index.html', import.meta.url), 'utf8');
    const bootstrap = index.indexOf('<script src="/appearance-bootstrap.js"></script>');
    expect(bootstrap).toBeGreaterThan(index.indexOf('<head>'));
    expect(bootstrap).toBeLessThan(index.indexOf('</head>'));
    expect(bootstrap).toBeLessThan(index.indexOf('<script type="module" src="/src/main.ts">'));
    expect(bootstrapSource).toContain(`getItem('${APPEARANCE_PREFERENCE_KEY}')`);
  });
});
