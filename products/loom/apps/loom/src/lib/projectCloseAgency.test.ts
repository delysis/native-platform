import { describe, expect, it, vi } from 'vitest';
import {
  captureProjectCloseAgency,
  restoreProjectCloseAgency
} from './projectCloseAgency';

describe('project close agency restoration', () => {
  it('captures the normal authoring focus gate and exact suggestion preference', () => {
    expect(captureProjectCloseAgency(true)).toEqual({
      suggestionsEnabled: true,
      focusMode: false
    });
    expect(captureProjectCloseAgency(false)).toEqual({
      suggestionsEnabled: false,
      focusMode: false
    });
  });

  it('releases native focus before restoring the prior suggestion preference', async () => {
    const order: string[] = [];
    const setFocusMode = vi.fn(async (enabled: false) => {
      order.push(`focus:${enabled}`);
    });
    const setSuggestionsEnabled = vi.fn(async (enabled: boolean) => {
      order.push(`suggestions:${enabled}`);
    });

    await restoreProjectCloseAgency(captureProjectCloseAgency(true), {
      setFocusMode,
      setSuggestionsEnabled
    });

    expect(order).toEqual(['focus:false', 'suggestions:true']);
  });

  it('never claims restoration when native focus release fails', async () => {
    const failure = new Error('focus gate remained closed');
    const setSuggestionsEnabled = vi.fn(async (_enabled: boolean) => {});

    await expect(restoreProjectCloseAgency(captureProjectCloseAgency(true), {
      setFocusMode: vi.fn(async (_enabled: false) => { throw failure; }),
      setSuggestionsEnabled
    })).rejects.toBe(failure);

    expect(setSuggestionsEnabled).not.toHaveBeenCalled();
  });
});
