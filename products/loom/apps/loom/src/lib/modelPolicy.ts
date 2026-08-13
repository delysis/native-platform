import type {
  BuildModelPolicySummary,
  ModelCapabilitySummary,
  ModelPolicyProfile
} from './types';

export interface LocalWriterCandidate {
  modelPath: string;
  profileId: string;
  policyRank: number;
}

export type AutomaticWriterSummary = ModelCapabilitySummary & {
  local: true;
  loaded: true;
  header_verified: true;
  completion: true;
  output_tokens: true;
  policy_verified: ModelPolicyProfile;
};

const GEMMA_4_BASE_WRITER_PROFILE = 'gemma_4_e2b_base_q8_loom_v1';

export function writerProfileForBuildPolicy(
  policy: BuildModelPolicySummary | null
): string | null {
  switch (policy?.name) {
    case 'writer-gemma4-base-v1':
    case 'writer-gemma4-base-v2':
      return GEMMA_4_BASE_WRITER_PROFILE;
    case 'none-v1':
    case undefined:
      return null;
  }
}

function compareCodePoints(left: string, right: string): number {
  return left < right ? -1 : left > right ? 1 : 0;
}

export function orderedLocalWriterCandidates(
  models: readonly ModelCapabilitySummary[]
): LocalWriterCandidate[] {
  return models
    .filter((model) =>
      model.local &&
      model.header_verified &&
      !model.loaded &&
      Boolean(model.policy_candidate)
    )
    .map((model) => ({
      modelPath: model.model_path,
      profileId: model.policy_candidate!.profile_id,
      policyRank: model.policy_candidate!.rank
    }))
    .sort((left, right) =>
      left.policyRank - right.policyRank ||
      compareCodePoints(left.profileId, right.profileId) ||
      compareCodePoints(left.modelPath, right.modelPath)
    );
}

/**
 * Chooses only a resident model or a policy candidate for the writer picker.
 * A remembered arbitrary GGUF must never displace the compatible-model setup
 * path merely because its header can be parsed.
 */
export function preferredWriterModelPath(
  models: readonly ModelCapabilitySummary[],
  rememberedPath: string | null,
  currentPath: string
): string {
  const loaded = models.find((model) => model.loaded);
  if (loaded) return loaded.model_path;

  const candidates = orderedLocalWriterCandidates(models);
  const candidatePaths = new Set(candidates.map((candidate) => candidate.modelPath));
  if (rememberedPath && candidatePaths.has(rememberedPath)) return rememberedPath;
  if (candidatePaths.has(currentPath)) return currentPath;
  return candidates[0]?.modelPath ?? '';
}

export function isVerifiedPolicyWriter(
  model: ModelCapabilitySummary,
  profileId: string
): model is AutomaticWriterSummary {
  return model.loaded &&
    model.local &&
    model.header_verified &&
    model.policy_verified?.profile_id === profileId &&
    model.completion &&
    model.output_tokens;
}

/**
 * Select only a resident model whose exact native identity belongs to this
 * closed build policy. A generic loaded completion model remains visible to
 * Advanced controls but cannot become the automatic writer in renderer state.
 */
export function automaticWriterForBuildPolicy(
  models: readonly ModelCapabilitySummary[],
  policy: BuildModelPolicySummary | null
): AutomaticWriterSummary | undefined {
  const profileId = writerProfileForBuildPolicy(policy);
  return profileId
    ? models.find((model): model is AutomaticWriterSummary =>
      isVerifiedPolicyWriter(model, profileId))
    : undefined;
}
