import { mount, unmount } from 'svelte';
import { afterEach, expect, it } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import SourceEditor from './SourceEditor.svelte';
import { decodeVerseForEditor, encodeVerseFromEditor } from './verseCodec';
import { sourceCaretByte } from './sourceCaret';
import '../app.css';
let mounted: ReturnType<typeof mount> | undefined;
afterEach(async () => { if (mounted) await unmount(mounted); mounted = undefined; document.body.replaceChildren(); });

it('preserves CRLF prose through real textarea typing, exact caret capture, and reopen', async () => {
  let raw = 'Café\r\n🧵 second\r\n';
  const plain = document.createElement('textarea'); plain.value = raw;
  expect(plain.value).toBe('Café\n🧵 second\n');
  expect(raw.startsWith(plain.value.slice(0, 7))).toBe(false);
  async function open() {
    const decoded = decodeVerseForEditor(raw);
    const target = document.createElement('div'); document.body.append(target);
    mounted = mount(SourceEditor, { target, props: { element: undefined, value: decoded.display, verse: false, verseNewline: decoded.codec.newline,
      onValueInput: (area: HTMLTextAreaElement) => { raw = encodeVerseFromEditor(area.value, decoded.codec); } } });
    const locator = page.getByRole('textbox', { name: 'Markdown source editor' });
    await locator.click();
    return { area: locator.element() as HTMLTextAreaElement, codec: decoded.codec };
  }
  let { area, codec } = await open();
  area.setSelectionRange(7, 7);
  expect(sourceCaretByte(area.value, area.selectionStart, raw, codec)).toBe(new TextEncoder().encode('Café\r\n🧵').length);
  await userEvent.keyboard(' new');
  expect(raw).toBe('Café\r\n🧵 new second\r\n');
  expect(sourceCaretByte(area.value, area.selectionStart, raw, codec)).toBe(new TextEncoder().encode('Café\r\n🧵 new').length);
  await unmount(mounted!); mounted = undefined; document.body.replaceChildren();
  ({ area, codec } = await open());
  expect(area.value).toBe('Café\n🧵 new second\n');
  expect(sourceCaretByte(area.value, area.value.length, raw, codec)).toBe(new TextEncoder().encode(raw).length);
});
