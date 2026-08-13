import { afterEach, describe, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({ invoke: vi.fn() }));

vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));

import {
  confirmResearchPromotion,
  importResearchPromotion,
  listPendingResearchPromotions
} from './ipc';
import type { ResearchPromotionPrompt } from './types';

const priorWindow = globalThis.window;

afterEach(() => {
  mocks.invoke.mockReset();
  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: priorWindow
  });
});

function installDesktopRuntime(): void {
  Object.defineProperty(globalThis, 'window', {
    configurable: true,
    value: { __TAURI_INTERNALS__: {} }
  });
}

describe('research foreground promotion IPC', () => {
  it('requests a native research-packet import without renderer-supplied bytes or paths', async () => {
    installDesktopRuntime();
    mocks.invoke.mockResolvedValue(null);

    await expect(importResearchPromotion('project', 'session')).resolves.toBeNull();
    expect(mocks.invoke).toHaveBeenCalledWith(
      'plugin:loom|research_promotion_import',
      { projectId: 'project', sessionId: 'session' }
    );
  });

  it('lists only host-staged pending decisions for the exact session', async () => {
    installDesktopRuntime();
    mocks.invoke.mockResolvedValue([]);

    await expect(listPendingResearchPromotions('project', 'session')).resolves.toEqual([]);
    expect(mocks.invoke).toHaveBeenCalledWith(
      'plugin:loom|research_promotion_pending',
      { projectId: 'project', sessionId: 'session' }
    );
  });

  it('submits the exact opaque challenge and binding as one nested command input', async () => {
    installDesktopRuntime();
    const prompt: ResearchPromotionPrompt = {
      command_id: 'command',
      nonce: 'nonce',
      document_id: 'document',
      candidate_fingerprint: 'a'.repeat(64),
      promotion_fingerprint: 'b'.repeat(64),
      subject_kind: 'candidate_projection',
      expires_at_unix_ms: 1,
      result_text: 'Reviewed result'
    };
    mocks.invoke.mockResolvedValue({ receipt: {}, foreground_receipt_blob_id: 'c'.repeat(64) });

    await confirmResearchPromotion('project', 'session', prompt);

    expect(mocks.invoke).toHaveBeenCalledWith(
      'plugin:loom|research_promotion_confirm',
      {
        projectId: 'project',
        sessionId: 'session',
        input: {
          command_id: 'command',
          nonce: 'nonce',
          document_id: 'document',
          candidate_fingerprint: 'a'.repeat(64),
          promotion_fingerprint: 'b'.repeat(64)
        }
      }
    );
  });
});
