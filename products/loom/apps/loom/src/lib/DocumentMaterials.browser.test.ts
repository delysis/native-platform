import { mount, unmount } from 'svelte';
import { describe, expect, it, vi } from 'vitest';
import { page, userEvent } from 'vitest/browser';
import '../app.css';
import DocumentMaterials from './DocumentMaterials.svelte';
import type { ContextAttachmentPresentation } from './types';

const material: ContextAttachmentPresentation = {
  id: 'a'.repeat(64), source_revision: 'b'.repeat(64), excerpt: null,
  file_name: 'Source 🖋.md', detected_format: 'markdown', coverage_complete: true,
  text_bytes: 80, presentation_kind: 'text', media: [], warnings: []
};

describe('explicit document materials', () => {
  it('edits a source excerpt without rewriting matching authored instructions', async () => {
    const target = document.createElement('div'); target.style.width = '220px'; document.body.append(target);
    const onInstructions = vi.fn(); const onSave = vi.fn(async () => true); const onRemove = vi.fn();
    const instructions = 'Même phrase 🖋. Preserve my instructions.';
    const component = mount(DocumentMaterials, {target, props: {title:'Draft', instructions, attachments:[material], onInstructions, onFlush:vi.fn(), onRemove, onSave}});
    try {
      await page.getByText('Materials (1)', {exact:true}).click();
      await page.getByText('Original source', {exact:true}).click();
      const excerpt = 'Même phrase 🖋.\nEdited material stays material.';
      await userEvent.fill(page.getByRole('textbox',{name:'Excerpt for Source 🖋.md'}), excerpt);
      await page.getByRole('button',{name:'Use excerpt',exact:true}).click();
      expect(onSave).toHaveBeenCalledWith(material.id, material.source_revision, excerpt);
      expect(onInstructions).not.toHaveBeenCalled();
      await expect.element(page.getByRole('textbox',{name:'Instructions',exact:true})).toHaveValue(instructions);
      expect(target.scrollWidth).toBeLessThanOrEqual(220);
      await page.getByRole('button',{name:'Remove Source 🖋.md from materials'}).click();
      expect(onRemove).toHaveBeenCalledWith(material.id);
    } finally { await unmount(component); target.remove(); }
  });

  it('keeps an incompatible context error separate from manuscript editing', async () => {
    const target = document.createElement('div'); document.body.append(target);
    const component = mount(DocumentMaterials, {target,props:{title:'Draft',instructions:'',attachments:[],error:'Saved context uses an earlier mixed-text format.',onInstructions:vi.fn(),onFlush:vi.fn(),onRemove:vi.fn(),onSave:async()=>false}});
    try {
      await page.getByText('Materials',{exact:true}).click();
      await expect.element(page.getByRole('status')).toHaveTextContent('earlier mixed-text format');
      await expect.element(page.getByText('Your document remains editable. Saved context has not been rewritten.',{exact:true})).toBeVisible();
      expect(page.getByRole('textbox',{name:'Instructions',exact:true}).query()).toBeNull();
    } finally { await unmount(component); target.remove(); }
  });
});
