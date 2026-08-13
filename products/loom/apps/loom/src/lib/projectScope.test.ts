import { describe, expect, it } from 'vitest';
import {
  navigationScopeIsCurrent,
  projectRestoreScopeIsCurrent,
  projectSessionIsCurrent
} from './projectScope';

const scope = {
  projectId: 'project-a',
  sessionId: 'session-1',
  restoreSerial: 3,
  documentEpoch: 7,
  editVersion: 11,
  documentId: 'document-a'
};

describe('projectSessionIsCurrent', () => {
  it('rejects a closed project and a replacement session for the same project', () => {
    expect(projectSessionIsCurrent(null, scope)).toBe(false);
    expect(projectSessionIsCurrent({
      project_id: 'project-a',
      session_id: 'session-2'
    }, scope)).toBe(false);
  });

  it('accepts only the exact bound project session', () => {
    expect(projectSessionIsCurrent({
      project_id: 'project-a',
      session_id: 'session-1'
    }, scope)).toBe(true);
  });
});

describe('navigationScopeIsCurrent', () => {
  const project = { project_id: 'project-a', session_id: 'session-1' };
  const document = { summary: { document_id: 'document-a' } };

  it('requires the exact session, document, epoch, and edit version', () => {
    expect(navigationScopeIsCurrent(project, document, 7, 11, 3, scope)).toBe(true);
    expect(navigationScopeIsCurrent(project, document, 8, 11, 3, scope)).toBe(false);
    expect(navigationScopeIsCurrent(project, document, 7, 12, 3, scope)).toBe(false);
    expect(navigationScopeIsCurrent(project, {
      summary: { document_id: 'document-b' }
    }, 7, 11, 3, scope)).toBe(false);
    expect(navigationScopeIsCurrent({
      project_id: 'project-a',
      session_id: 'session-2'
    }, document, 7, 11, 3, scope)).toBe(false);
    expect(navigationScopeIsCurrent(project, document, 7, 11, 4, scope)).toBe(false);
  });
});

describe('projectRestoreScopeIsCurrent', () => {
  it('rejects a reply from an older restore cycle even when project and session match', () => {
    const project = { project_id: 'project-a', session_id: 'session-1' };
    expect(projectRestoreScopeIsCurrent(project, 3, scope)).toBe(true);
    expect(projectRestoreScopeIsCurrent(project, 4, scope)).toBe(false);
  });
});
