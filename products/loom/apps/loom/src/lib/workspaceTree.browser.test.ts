import { afterEach, describe, expect, it } from 'vitest';
import { workspaceCopyDestination } from './workspaceTree';

afterEach(() => document.body.replaceChildren());

describe('physical file drop destination', () => {
  it('resolves actual hit elements inside a folder and active root while rejecting an inactive root and outside targets', () => {
    const outline = document.createElement('nav');
    outline.style.cssText = 'position:fixed;left:0;top:0;width:260px;height:300px;';
    outline.innerHTML = '<button class="workspace-root" data-copy-folder="" style="display:block;width:260px;height:40px"><span>Active</span></button><button class="workspace-root" style="display:block;width:260px;height:40px"><span>Inactive</span></button><button data-copy-folder="Notes/Clippings/" style="display:block;width:260px;height:40px"><span>Clippings</span></button>';
    document.body.append(outline);
    const buttons = [...outline.querySelectorAll('button')];
    const hit = (element: Element) => {
      const bounds = element.getBoundingClientRect();
      return document.elementFromPoint(bounds.x + bounds.width / 2, bounds.y + bounds.height / 2);
    };
    expect(workspaceCopyDestination(hit(buttons[0]), outline)).toBe('');
    expect(workspaceCopyDestination(buttons[0].querySelector('span'), outline)).toBe('');
    expect(workspaceCopyDestination(hit(buttons[1]), outline)).toBeNull();
    expect(workspaceCopyDestination(buttons[1].querySelector('span'), outline)).toBeNull();
    expect(workspaceCopyDestination(hit(buttons[2]), outline)).toBe('Notes/Clippings/');
    expect(workspaceCopyDestination(document.elementFromPoint(200, 240), outline)).toBe('');
    const outside = document.createElement('button'); outside.dataset.copyFolder = 'Elsewhere/'; document.body.append(outside);
    expect(workspaceCopyDestination(outside, outline)).toBeNull();
    expect(workspaceCopyDestination(null, outline)).toBeNull();
  });
});
