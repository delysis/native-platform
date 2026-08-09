import type { ApplicationCloseOutcome } from './applicationCloseCoordinator';

export interface ApplicationCloseRetryClock {
  schedule: (callback: () => void, delayMs: number) => number;
  cancel: (handle: number) => void;
}

/** Owns one retry intent and invalidates it when any newer close signal arrives. */
export class ApplicationCloseRetryScheduler {
  private epoch = 0;
  private timer: number | undefined;

  constructor(
    private readonly clock: ApplicationCloseRetryClock,
    private readonly delayMs: number
  ) {}

  beginAttempt(): number {
    this.epoch += 1;
    this.cancelTimer();
    return this.epoch;
  }

  settle(
    attemptEpoch: number,
    outcome: ApplicationCloseOutcome,
    retry: () => void
  ): void {
    if (attemptEpoch !== this.epoch) return;
    this.cancelTimer();
    switch (outcome.status) {
      case 'quiesced': {
        const retryEpoch = this.epoch;
        this.timer = this.clock.schedule(() => {
          if (retryEpoch !== this.epoch) return;
          this.timer = undefined;
          retry();
        }, this.delayMs);
        return;
      }
      case 'exit_requested':
      case 'resumed':
      case 'failed':
      case 'lifecycle_unknown':
        return;
      default: {
        const unreachable: never = outcome;
        return unreachable;
      }
    }
  }

  dispose(): void {
    this.epoch += 1;
    this.cancelTimer();
  }

  private cancelTimer(): void {
    if (this.timer === undefined) return;
    this.clock.cancel(this.timer);
    this.timer = undefined;
  }
}
