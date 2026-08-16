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

export type SuggestionWriterSummary = ModelCapabilitySummary & {
  local: true;
  loaded: true;
  header_verified: true;
  completion: true;
  output_tokens: true;
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
 * A model explicitly loaded through the native runtime may power suggestions
 * once its descriptor proves text completion and generated-token output. Media
 * adapters stay out of this path; they are not standalone language models.
 */
export function isUsableSuggestionWriter(
  model: ModelCapabilitySummary
): model is SuggestionWriterSummary {
  return model.loaded &&
    model.local &&
    model.header_verified &&
    model.completion &&
    model.output_tokens &&
    model.projector_present === false &&
    model.media_kinds.length === 0;
}

/**
 * Select only a resident model whose exact native identity belongs to this
 * closed build policy. This strict selector powers quiet/default loading;
 * `suggestionWriter` separately recognizes an explicitly loaded text model.
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

/**
 * Prefer the exact tested writer when it is resident, then accept an
 * explicitly loaded native-verified text model. Generic models are never
 * selected by quiet discovery; this function only recognizes the one the
 * author has already chosen and loaded.
 */
export function suggestionWriter(
  models: readonly ModelCapabilitySummary[],
  policy: BuildModelPolicySummary | null
): SuggestionWriterSummary | undefined {
  return automaticWriterForBuildPolicy(models, policy) ??
    models.find(isUsableSuggestionWriter);
}

export function looksLikeVisionAdapter(model: ModelCapabilitySummary): boolean {
  return model.projector_present === true ||
    model.media_kinds.length > 0 ||
    /(^|[-_.])(mmproj|projector)([-_.]|$)/iu.test(model.display_name) ||
    /(^|[/\\-_.])(mmproj|projector)([/\\-_.]|$)/iu.test(model.model_path);
}
