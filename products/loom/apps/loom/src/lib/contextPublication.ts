import { invoke } from '@tauri-apps/api/core';

export interface ContextPublicationRequest {
  source: string;
  publication: string;
  fingerprint: string;
}
export interface ContextPublicationReview {
  request: ContextPublicationRequest;
  path: string;
  material: { markdown: string; files: { id: string; name: string; byte_count: number; markdown: string }[] };
  members: string[];
  started: boolean;
  published: boolean;
}
export interface PublishedContext { document_id: string; path: string; reference: string }
export interface ContextPublicationScope { projectId: string; sessionId: string; documentId: string }

export function reviewContext(scope: ContextPublicationScope): Promise<ContextPublicationReview> {
  return invoke('plugin:loom|cabal_context_review', { ...scope });
}
export function publishContext(scope: ContextPublicationScope, request: ContextPublicationRequest): Promise<PublishedContext> {
  return invoke('plugin:loom|cabal_context_publish', { projectId: scope.projectId, sessionId: scope.sessionId, request });
}
