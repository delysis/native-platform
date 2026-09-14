import type { WorkspacePaneConfig } from './WorkspacePane.svelte';
export interface WorkspaceTemplateSnapshot {
  enabled: boolean;
  document_id: string | null;
  revision_id: string | null;
  source_sha256: string | null;
  suggestions: boolean | null;
  model_path: string | null;
  config: { panes: Record<string, WorkspacePaneConfig> };
  error: string | null;
}
export interface SetupChoices {
  chat: boolean;
  suggestions: boolean;
}
