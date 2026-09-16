import { mount, unmount } from 'svelte';
import { afterEach, describe, expect, it } from 'vitest';
import { page } from 'vitest/browser';
import ChatTurn from './ChatTurn.svelte';
import type { TerminalRun } from './types';
import '../app.css';

let mounted: ReturnType<typeof mount> | undefined;
afterEach(async () => {
  if (mounted) await unmount(mounted);
  mounted = undefined;
  document.body.replaceChildren();
});
const base: TerminalRun = {
  run_id:'turn', status:'completed', expression:'raw',
  presentation:{ pane_id:'chat', input:'How does the moon keep its garden?' },
  output_document_id:'answer', output_relative_path:'Answer.md',
  preview:'', error:null, created_at_ms:1
};
function render(output: string, run: TerminalRun = base) {
  const target = document.createElement('div');
  target.style.cssText='width:360px;padding:14px;font-size:14px;background:var(--paper);color:var(--ink)';
  document.body.append(target);
  mounted = mount(ChatTurn, { target, props:{ run, output } });
  return target;
}

describe('readable chat turns', () => {
  it('folds explicit thinking independently of Markdown answers and expands with the keyboard', async () => {
    render('<think>Consider the impossible garden.\n\nThen imagine the moon watering it.</think>It waters the **stars**.\n\n- Silver petals\n- Quiet roots\n\n```text\nmoon → garden\n```');
    const thought = page.getByText('Consider the impossible garden.', { exact: true });
    await expect.element(thought).not.toBeVisible();
    expect(document.querySelector('.output strong')?.textContent).toBe('stars');
    expect(document.querySelectorAll('.output li')).toHaveLength(2);
    expect(document.querySelector('.output pre')?.textContent).toContain('moon → garden');
    const summary = document.querySelector('summary')!;
    summary.focus();
    const { userEvent } = await import('vitest/browser');
    await userEvent.keyboard('{Enter}');
    await expect.element(thought).toBeVisible();
    await userEvent.keyboard('{Enter}');
    await expect.element(thought).not.toBeVisible();
    const query = document.querySelector('.query')!;
    const answer = document.querySelector('.response')!;
    expect(query.getBoundingClientRect().left).toBeGreaterThan(answer.getBoundingClientRect().left);
    expect(getComputedStyle(query).backgroundColor).not.toBe('rgba(0, 0, 0, 0)');
  });

  it('keeps HTML inert, blocks generated remote images, and preserves source links', async () => {
    render('<script>window.bad = true</script>\n\n![remote](https://example.com/tracker.png)\n\n[unsafe](javascript:alert(1)) [disk](file:///etc/passwd) [source](loom-material:material-' + 'a'.repeat(64) + ')');
    await expect.poll(() => document.querySelector('.output')?.textContent).toContain('window.bad');
    expect(document.querySelector('.output script')).toBeNull();
    expect(document.querySelector('img')).toBeNull();
    expect(document.querySelector('a[href^="javascript:"]')).toBeNull();
    expect(document.querySelector('a[href^="file:"]')).toBeNull();
    expect(document.querySelector('a[href^="loom-material:"]')).not.toBeNull();
  });

  it('renders only recorded activity and exposes details without inventing a thinking trace', async () => {
    render('A small constellation.', { ...base, events:[{ kind:'search', label:'Searched Moon notes', detail:'Query: moon\n3 passages retained' }] });
    expect(document.querySelector('.thinking')).toBeNull();
    expect(document.querySelectorAll('.activity')).toHaveLength(1);
    await page.getByText('Searched Moon notes', { exact:true }).click();
    await expect.element(page.getByText('Query: moon\n3 passages retained', { exact:true })).toBeVisible();
    expect(document.querySelector('[role=status]')).toBeNull();
  });

  it('distinguishes cancellation and incomplete reasoning from a successful answer', async () => {
    render('<think>Not finished yet', { ...base, status:'cancelled' });
    await expect.element(page.getByRole('status')).toHaveTextContent('Stopped');
    expect(document.querySelector('.output')).toBeNull();
    await page.getByText('Thinking', { exact:true }).click();
    await expect.element(page.getByText('Thinking ended before a response.', { exact:true })).toBeVisible();
  });

  it('stays within a narrow pane even with long code and identifiers', async () => {
    const target = render('```text\n' + 'x'.repeat(300) + '\n```\n\n' + 'y'.repeat(300));
    await expect.poll(() => document.querySelector('pre')).not.toBeNull();
    expect(target.scrollWidth).toBeLessThanOrEqual(target.clientWidth);
    expect(document.querySelector('pre')!.scrollWidth).toBeGreaterThan(document.querySelector('pre')!.clientWidth);
  });
});
