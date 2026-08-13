import { describe, expect, it } from 'vitest';
import {
  emptyAutocompleteRetryLedger,
  planAutocompleteRetry
} from './autocompleteRetry';
import type { AutocompleteDisposition } from './ghostSuggestion';

const exhausted: AutocompleteDisposition = {
  kind: 'exhausted',
  candidates: [
    { candidateId: 'candidate-1', reason: 'repetition' },
    { candidateId: 'candidate-2', reason: 'too_short' }
  ]
};

describe('planAutocompleteRetry', () => {
  it('schedules exactly one replacement family for an exhausted scope', () => {
    const first = planAutocompleteRetry(emptyAutocompleteRetryLedger(), {
      disposition: exhausted,
      budgetKey: 'document:revision:model',
      activeBranchCount: 0,
      weaveStarting: false,
      maximumRetries: 1
    });
    expect(first.kind).toBe('schedule');

    const second = planAutocompleteRetry(first.ledger, {
      disposition: exhausted,
      budgetKey: 'document:revision:model',
      activeBranchCount: 0,
      weaveStarting: false,
      maximumRetries: 1
    });
    expect(second.kind).toBe('none');
    expect(second.ledger.attemptsByBudget['document:revision:model']).toBe(1);
  });

  it('does not spend budget while work or hydration is pending', () => {
    for (const disposition of [
      { kind: 'awaiting_candidates' } as const,
      { kind: 'awaiting_hydration', runIds: ['run-1'] } as const,
      { kind: 'inactive' } as const
    ]) {
      const decision = planAutocompleteRetry(emptyAutocompleteRetryLedger(), {
        disposition,
        budgetKey: 'scope',
        activeBranchCount: 0,
        weaveStarting: false,
        maximumRetries: 1
      });
      expect(decision).toEqual({ kind: 'none', ledger: { attemptsByBudget: {} } });
    }
    expect(planAutocompleteRetry(emptyAutocompleteRetryLedger(), {
      disposition: exhausted,
      budgetKey: 'scope',
      activeBranchCount: 1,
      weaveStarting: false,
      maximumRetries: 1
    }).kind).toBe('none');
  });

  it('retains spent budgets when the author alternates scopes', () => {
    const spent = { attemptsByBudget: { old: 1, other: 1 } };
    const oldAgain = planAutocompleteRetry(spent, {
      disposition: exhausted,
      budgetKey: 'old',
      activeBranchCount: 0,
      weaveStarting: false,
      maximumRetries: 1
    });
    expect(oldAgain).toEqual({ kind: 'none', ledger: spent });

    const newBudget = planAutocompleteRetry(spent, {
      disposition: exhausted,
      budgetKey: 'new',
      activeBranchCount: 0,
      weaveStarting: false,
      maximumRetries: 1
    });
    expect(newBudget.kind).toBe('schedule');
    expect(newBudget.ledger.attemptsByBudget).toEqual({ old: 1, other: 1, new: 1 });
  });
});
