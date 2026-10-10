import { describe, expect, it, vi } from 'vitest';
import { resolveScopedMaterialReference, type MaterialReferenceResolution } from './materialEvidenceScope';
import type { MaterialEvidence } from './materials';

const active = { projectId: 'manuscript', sessionId: 'root-session' };
const owner = { projectId: 'workspace', sessionId: 'workspace-session' };
const evidence = { id: 'receipt' } as MaterialEvidence;
const result: MaterialReferenceResolution = { project_id: owner.projectId, session_id: owner.sessionId, material: null, evidence };

describe('atomic source reference resolution', () => {
  it('keeps the native-selected owner scope and retained evidence even after source removal', async () => {
    const resolve = vi.fn().mockResolvedValue(result);
    expect(await resolveScopedMaterialReference(active, owner, 'evidence/receipt', () => true, resolve)).toEqual(result);
    expect(resolve.mock.calls).toEqual([['manuscript', 'root-session', 'evidence/receipt']]);
  });

  it('does not infer physical store identity from matching project IDs or different session tokens', async () => {
    const sameProjectActive = { ...active, projectId: owner.projectId };
    const resolve = vi.fn().mockResolvedValue(result);
    expect(await resolveScopedMaterialReference(sameProjectActive, owner, 'evidence/receipt', () => true, resolve)).toEqual(result);
    resolve.mockResolvedValue({ ...result, session_id: 'unrelated-session' });
    await expect(resolveScopedMaterialReference(sameProjectActive, owner, 'evidence/receipt', () => true, resolve)).rejects.toThrow('does not belong');
  });

  it.each(['material_ambiguous', 'material_failed', 'material_source_corrupt', 'project_session_mismatch', 'material_not_found'])('propagates %s without selecting a fallback origin', async code => {
    const resolve = vi.fn().mockRejectedValue({ code });
    await expect(resolveScopedMaterialReference(active, owner, 'materials/source', () => true, resolve)).rejects.toEqual({ code });
    expect(resolve).toHaveBeenCalledTimes(1);
  });

  it('discards an in-flight result when the clicked root is no longer current', async () => {
    let current = true;
    let finish!: (value: MaterialReferenceResolution) => void;
    const resolve = vi.fn(() => new Promise<MaterialReferenceResolution>(done => { finish = done; }));
    const pending = resolveScopedMaterialReference(active, owner, 'evidence/receipt', () => current, resolve);
    current = false;
    finish(result);
    expect(await pending).toBeNull();
    expect(resolve).toHaveBeenCalledTimes(1);
    expect(await resolveScopedMaterialReference(active, owner, 'evidence/receipt', () => false, resolve)).toBeNull();
    expect(resolve).toHaveBeenCalledTimes(1);
  });

  it('rejects an empty native response instead of opening an unrelated cached source', async () => {
    const resolve = vi.fn().mockResolvedValue({ ...result, evidence: null });
    await expect(resolveScopedMaterialReference(active, owner, 'material-missing', () => true, resolve)).rejects.toThrow('does not belong');
  });
});
