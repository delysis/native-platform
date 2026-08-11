import type { AutocompleteDisposition } from './ghostSuggestion';

export interface AutocompleteRetryLedger {
  attemptsByBudget: Readonly<Record<string, number>>;
}

export type AutocompleteRetryDecision =
  | { kind: 'none'; ledger: AutocompleteRetryLedger }
  | { kind: 'schedule'; ledger: AutocompleteRetryLedger };

export interface AutocompleteRetryInput {
  disposition: AutocompleteDisposition;
  budgetKey: string;
  activeBranchCount: number;
  weaveStarting: boolean;
  maximumRetries: number;
}

export const emptyAutocompleteRetryLedger = (): AutocompleteRetryLedger => ({
  attemptsByBudget: {}
});

/**
 * Admit at most `maximumRetries` replacement families for each caller-defined
 * live budget. The ledger retains every spent key for the component lifetime,
 * so alternating carets cannot refill an immutable revision's allowance. Only
 * a fully hydrated, fully rejected family spends the budget.
 */
export function planAutocompleteRetry(
  previous: AutocompleteRetryLedger,
  input: AutocompleteRetryInput
): AutocompleteRetryDecision {
  const attempts = previous.attemptsByBudget[input.budgetKey] ?? 0;
  if (
    !input.budgetKey ||
    input.disposition.kind !== 'exhausted' ||
    input.disposition.candidates.length === 0 ||
    input.activeBranchCount !== 0 ||
    input.weaveStarting ||
    !Number.isSafeInteger(input.maximumRetries) ||
    input.maximumRetries < 0 ||
    attempts >= input.maximumRetries
  ) return { kind: 'none', ledger: previous };

  return {
    kind: 'schedule',
    ledger: {
      attemptsByBudget: {
        ...previous.attemptsByBudget,
        [input.budgetKey]: attempts + 1
      }
    }
  };
}
