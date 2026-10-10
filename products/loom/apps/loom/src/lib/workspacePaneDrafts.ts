import { writable, type Writable } from 'svelte/store';
import type { LoomFailure, OpenDocument, TerminalRun } from './types';

export type WorkspacePaneSubmission =
  | { status: 'accepted'; run: TerminalRun }
  | { status: 'rejected'; error: LoomFailure };

export interface WorkspacePaneRunRequest {
  workspace_id: string;
  workspace_session_id: string;
  command_id: string;
  pane_id: string;
  configuration_revision_id: string;
  expression: string;
  input: string;
  captured_document?: {
    project_id: string;
    session_id: string;
    document_id: string;
    revision_id: string;
    visible_blob_id: string;
  };
}

export interface WorkspacePanePreparation { document: OpenDocument | null }

/** Owned by the workspace, not the document root currently shown in its editor. */
export interface WorkspacePaneDraft {
  entry: string;
  pending: WorkspacePaneRunRequest | null;
  failure: string | null;
}

export type WorkspacePaneDraftStore = Writable<WorkspacePaneDraft>;

export function createWorkspacePaneDraft(): WorkspacePaneDraftStore {
  return writable({ entry: '', pending: null, failure: null });
}

export class WorkspacePaneDrafts {
  private session = '';
  private readonly panes = new Map<string, WorkspacePaneDraftStore>();

  forPane(workspaceSession: string, paneId: string): WorkspacePaneDraftStore {
    if (this.session !== workspaceSession) {
      this.session = workspaceSession;
      this.panes.clear();
    }
    let draft = this.panes.get(paneId);
    if (!draft) {
      draft = createWorkspacePaneDraft();
      this.panes.set(paneId, draft);
    }
    return draft;
  }
}
