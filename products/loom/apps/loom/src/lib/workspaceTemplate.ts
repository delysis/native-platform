import type { WorkspacePaneConfig } from './WorkspacePane.svelte';
export interface WorkspaceTemplateSnapshot {
  enabled: boolean;
  document_id: string | null;
  revision_id: string | null;
  config: { panes: Record<string, WorkspacePaneConfig> };
  error: string | null;
}
