import type { ModelCapabilitySummary } from './types';

export interface LocalWriterCandidate {
  modelPath: string;
  profileId: string;
  policyRank: number;
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

export function isVerifiedPolicyWriter(
  model: ModelCapabilitySummary,
  profileId: string
): boolean {
  return model.loaded &&
    model.local &&
    model.header_verified &&
    model.policy_verified?.profile_id === profileId &&
    model.completion &&
    model.output_tokens;
}
