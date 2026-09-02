import { describe, expect, it, vi } from 'vitest';
import { routeDocumentFilesystemHint } from './documentFilesystemHint';

describe('native document filesystem hints', () => {
  const currentProject = {
    project_id: 'project-current',
    session_id: 'session-current'
  };

  it('routes an exact-session hint only into the refresh scheduler', () => {
    const scheduleRefresh = vi.fn();

    expect(routeDocumentFilesystemHint(currentProject, currentProject, scheduleRefresh)).toBe(true);
    expect(scheduleRefresh).toHaveBeenCalledOnce();
    expect(scheduleRefresh).toHaveBeenCalledWith();
  });

  it('drops stale project and session hints without scheduling work', () => {
    const scheduleRefresh = vi.fn();

    expect(routeDocumentFilesystemHint({
      project_id: 'project-stale',
      session_id: currentProject.session_id
    }, currentProject, scheduleRefresh)).toBe(false);
    expect(routeDocumentFilesystemHint({
      project_id: currentProject.project_id,
      session_id: 'session-stale'
    }, currentProject, scheduleRefresh)).toBe(false);
    expect(routeDocumentFilesystemHint(currentProject, null, scheduleRefresh)).toBe(false);
    expect(routeDocumentFilesystemHint(null, currentProject, scheduleRefresh)).toBe(false);
    expect(routeDocumentFilesystemHint({
      project_id: 42,
      session_id: currentProject.session_id
    }, currentProject, scheduleRefresh)).toBe(false);
    expect(routeDocumentFilesystemHint({
      project_id: currentProject.project_id,
      session_id: null
    }, currentProject, scheduleRefresh)).toBe(false);
    expect(scheduleRefresh).not.toHaveBeenCalled();
  });

  it('forwards repeated matching events to the same scheduler boundary', () => {
    const scheduleRefresh = vi.fn();

    routeDocumentFilesystemHint(currentProject, currentProject, scheduleRefresh);
    routeDocumentFilesystemHint(currentProject, currentProject, scheduleRefresh);
    routeDocumentFilesystemHint(currentProject, currentProject, scheduleRefresh);

    expect(scheduleRefresh).toHaveBeenCalledTimes(3);
  });
});
