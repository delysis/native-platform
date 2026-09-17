import type { WorkspacePaneConfig } from './WorkspacePane.svelte';
import type { ConfiguredModelDownload } from './modelDownload';
export type WorkspaceModelSelection = { catalog: string } | { profile: string };

export interface WorkspaceTemplateSnapshot {
  enabled: boolean;
  document_id: string | null;
  revision_id: string | null;
  source_sha256: string | null;
  suggestions: boolean | null;
  model_path: string | null;
  downloads: Record<string, ConfiguredModelDownload>;
  google_client_configured: boolean;
  config: { model?: WorkspaceModelSelection | null; theme?: { mode: 'system' | 'light' | 'dark'; canvas?: string | null; text?: string | null; accent?: string | null }; panes: Record<string, WorkspacePaneConfig> };
  error: string | null;
}

import { isVerifiedCatalogWriter, legacyLocalCatalogMatch } from './modelCatalog';
import { isUsableSuggestionWriter, isVerifiedPolicyWriter, startupWriterCandidates, suggestionWriter, type StartupWriterCandidate, type SuggestionWriterSummary } from './modelPolicy';
import type { BuildModelPolicySummary, CuratedModelCatalogEntry, ModelCapabilitySummary } from './types';

export function workspaceWriterModel(
  models: readonly ModelCapabilitySummary[],
  policy: BuildModelPolicySummary | null,
  catalog: readonly CuratedModelCatalogEntry[],
  template: WorkspaceTemplateSnapshot | null,
  scopeCurrent: boolean
): SuggestionWriterSummary | undefined {
  if (!scopeCurrent || !template || template.error) return undefined;
  const selection = template.config.model;
  const available = template.model_path ? models.filter(model => model.model_path === template.model_path) : models;
  if (!selection) return suggestionWriter(available, policy);
  return available.find((model): model is SuggestionWriterSummary =>
    isUsableSuggestionWriter(model) && ('profile' in selection
      ? isVerifiedPolicyWriter(model, selection.profile)
      : catalog.some((entry) => entry.catalog_id === selection.catalog && isVerifiedCatalogWriter(entry, model))));
}

export interface WorkspaceWriterCandidate extends StartupWriterCandidate { catalogId?: string }

/** Discovery is only a hint. Every explicit candidate still needs native exact-identity admission. */
export function workspaceWriterCandidates(
  selection: WorkspaceModelSelection | null | undefined,
  models: readonly ModelCapabilitySummary[],
  catalog: readonly CuratedModelCatalogEntry[],
  rememberedPath: string | null
): WorkspaceWriterCandidate[] {
  if (!selection) return startupWriterCandidates(models, rememberedPath);
  const entry = 'catalog' in selection ? catalog.find((item) => item.catalog_id === selection.catalog) : undefined;
  return models.filter((model) => model.local && model.header_verified && ('profile' in selection
    ? model.policy_candidate?.profile_id === selection.profile || model.policy_verified?.profile_id === selection.profile
    : Boolean(entry && (legacyLocalCatalogMatch(entry, model) || isVerifiedCatalogWriter(entry, model)))))
    .map((model) => ({ modelPath: model.model_path, profileId: 'profile' in selection ? selection.profile : null,
      ...('catalog' in selection ? { catalogId: selection.catalog } : {}), policyRank: 0, remembered: false }))
    .sort((left, right) => left.modelPath.localeCompare(right.modelPath));
}

export interface SetupChoices {
  chat: boolean;
  suggestions: boolean;
}
