import { describe, expect, it, vi } from 'vitest';
import { ApplicationCloseRetryScheduler } from './applicationCloseRetry';

function fakeClock() {
  let nextHandle = 1;
  const callbacks = new Map<number, () => void>();
  return {
    clock: {
      schedule(callback: () => void, _delayMs: number): number {
        const handle = nextHandle;
        nextHandle += 1;
        callbacks.set(handle, callback);
        return handle;
      },
      cancel(handle: number): void {
        callbacks.delete(handle);
      }
    },
    runAll(): void {
      const ready = [...callbacks.values()];
      callbacks.clear();
      for (const callback of ready) callback();
    },
    pendingCount(): number {
      return callbacks.size;
    }
  };
}

describe('ApplicationCloseRetryScheduler', () => {
  it('invalidates an older quiesced retry when a duplicate event definitively resumes', () => {
    const clock = fakeClock();
    const retry = vi.fn();
    const scheduler = new ApplicationCloseRetryScheduler(clock.clock, 300);

    const first = scheduler.beginAttempt();
    scheduler.settle(first, { status: 'quiesced' }, retry);
    expect(clock.pendingCount()).toBe(1);

    const duplicate = scheduler.beginAttempt();
    scheduler.settle(duplicate, { status: 'resumed' }, retry);
    expect(clock.pendingCount()).toBe(0);
    clock.runAll();
    expect(retry).not.toHaveBeenCalled();
  });

  it('runs one current quiesced retry and cancels it on disposal', () => {
    const clock = fakeClock();
    const retry = vi.fn();
    const scheduler = new ApplicationCloseRetryScheduler(clock.clock, 300);

    const first = scheduler.beginAttempt();
    scheduler.settle(first, { status: 'quiesced' }, retry);
    clock.runAll();
    expect(retry).toHaveBeenCalledTimes(1);

    const second = scheduler.beginAttempt();
    scheduler.settle(second, { status: 'quiesced' }, retry);
    scheduler.dispose();
    clock.runAll();
    expect(retry).toHaveBeenCalledTimes(1);
  });
});
