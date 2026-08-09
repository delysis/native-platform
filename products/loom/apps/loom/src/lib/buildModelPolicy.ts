import type {
  BuildModelPolicyName,
  BuildModelPolicySummary,
  SuggestionActivation
} from './types';

type PolicyContractByName = {
  [Name in BuildModelPolicyName]: Omit<Extract<BuildModelPolicySummary, { name: Name }>, 'name'>;
};

const POLICY_CONTRACTS = {
  'none-v1': {
    activation: 'project_opt_in',
    canonical_sha256: 'ce3bdf5e3dbcac6f7bcc164ec4cc5c78b4a7b5bef7c49b3cd52c61e123b75fe0'
  },
  'writer-gemma4-base-v1': {
    activation: 'project_opt_in',
    canonical_sha256: 'c0492fb2285ad0922f89ab7288d63ef68fd17f5133f00ea4276622a15c2dc4e6'
  },
  'writer-gemma4-base-v2': {
    activation: 'quiet_default',
    canonical_sha256: '2d402d213b60ba65c4d018907e9eba67ccfbc1e97081cc0505f9713ae2dd89d2'
  }
} as const satisfies PolicyContractByName;

const POLICY_KEYS = ['activation', 'canonical_sha256', 'name'] as const;

function invalidPolicyContract(): never {
  throw {
    code: 'build_model_policy_contract_invalid',
    message: 'Loom could not verify its local suggestion policy.',
    retryable: false
  };
}

function isPolicyName(value: unknown): value is BuildModelPolicyName {
  return (
    typeof value === 'string' &&
    Object.prototype.hasOwnProperty.call(POLICY_CONTRACTS, value)
  );
}

/**
 * Treats the native response as untrusted input. A new or modified policy must
 * update this closed contract before it can influence suggestion activation.
 */
export function decodeBuildModelPolicy(value: unknown): BuildModelPolicySummary {
  if (value === null || typeof value !== 'object' || Array.isArray(value)) {
    return invalidPolicyContract();
  }
  const record = value as Record<string, unknown>;
  const keys = Object.keys(record).sort();
  if (
    keys.length !== POLICY_KEYS.length ||
    !keys.every((key, index) => key === POLICY_KEYS[index])
  ) {
    return invalidPolicyContract();
  }
  if (!isPolicyName(record.name)) {
    return invalidPolicyContract();
  }

  const expected = POLICY_CONTRACTS[record.name];
  if (
    record.activation !== expected.activation ||
    record.canonical_sha256 !== expected.canonical_sha256
  ) {
    return invalidPolicyContract();
  }

  return {
    name: record.name,
    activation: record.activation as SuggestionActivation,
    canonical_sha256: record.canonical_sha256
  } as BuildModelPolicySummary;
}
