export type ApplicationClosePhase = 'running' | 'closing';

export function applicationAllowsModelPreparation(phase: ApplicationClosePhase): boolean {
  switch (phase) {
    case 'running': return true;
    case 'closing': return false;
  }
  const unreachable: never = phase;
  return unreachable;
}

export type ApplicationCloseOutcome =
  | { status: 'exit_requested' }
  | { status: 'resumed' }
  | { status: 'quiesced' }
  | { status: 'failed'; error: unknown }
  | { status: 'lifecycle_unknown'; error: ApplicationCloseAbortFailure };

export type ApplicationStartupDisposition = 'continue' | 'hold_for_close';

export function applicationStartupDisposition(
  outcome: ApplicationCloseOutcome
): ApplicationStartupDisposition {
  switch (outcome.status) {
    case 'resumed':
    case 'failed':
      return 'continue';
    case 'exit_requested':
    case 'quiesced':
    case 'lifecycle_unknown':
      return 'hold_for_close';
    default: {
      const unreachable: never = outcome;
      return unreachable;
    }
  }
}

export type ProjectCloseOutcome =
  | { status: 'closed' }
  | { status: 'resume' }
  | { status: 'quiesced' };

export interface ApplicationCloseOperations {
  begin: () => boolean;
  closeProject: () => Promise<ProjectCloseOutcome>;
  authorizeNativeClose: () => Promise<void>;
  abortNativeClose: () => Promise<void>;
  reset: () => void;
  fail: (error: unknown) => void;
}

export interface ApplicationCloseAbortFailure {
  code: 'application_close_abort_failed';
  message: string;
  retryable: false;
  close_error: unknown;
  abort_error: unknown;
}

export function isApplicationCloseAbortFailure(
  error: unknown
): error is ApplicationCloseAbortFailure {
  if (typeof error !== 'object' || error === null) return false;
  return 'code' in error && error.code === 'application_close_abort_failed' &&
    'retryable' in error && error.retryable === false;
}

/**
 * Serializes every renderer-originated close signal into one native close.
 *
 * macOS can deliver a window close request and an application exit request for
 * the same gesture. Keeping an authorized or lifecycle-unknown promise latched
 * prevents either signal from invoking native lifecycle authority twice. A
 * resumed, draining, or recoverable failed attempt is released for an exact
 * retry.
 */
export class ApplicationCloseCoordinator {
  private attempt: Promise<ApplicationCloseOutcome> | null = null;

  constructor(private readonly operations: ApplicationCloseOperations) {}

  request(): Promise<ApplicationCloseOutcome> {
    if (this.attempt) return this.attempt;

    const attempt = this.run();
    this.attempt = attempt;
    void attempt.then((outcome) => {
      if (this.attempt !== attempt) return;
      switch (outcome.status) {
        case 'resumed':
        case 'quiesced':
        case 'failed':
          this.attempt = null;
          return;
        case 'exit_requested':
        case 'lifecycle_unknown':
          return;
        default: {
          const unreachable: never = outcome;
          return unreachable;
        }
      }
    });
    return attempt;
  }

  private async run(): Promise<ApplicationCloseOutcome> {
    try {
      if (!this.operations.begin()) return await this.abortAndResume();
      const project = await this.operations.closeProject();
      switch (project.status) {
        case 'resume': return await this.abortAndResume();
        case 'quiesced': return { status: 'quiesced' };
        case 'closed': break;
        default: {
          const unreachable: never = project;
          return unreachable;
        }
      }
      await this.operations.authorizeNativeClose();
      return { status: 'exit_requested' };
    } catch (error) {
      return await this.abortFailedClose(error);
    }
  }

  private async abortAndResume(): Promise<ApplicationCloseOutcome> {
    const abortFailure = await this.abortAndReset(undefined);
    return abortFailure ?? { status: 'resumed' };
  }

  private async abortFailedClose(closeError: unknown): Promise<ApplicationCloseOutcome> {
    const abortFailure = await this.abortAndReset(closeError);
    if (abortFailure) return abortFailure;
    this.operations.fail(closeError);
    return { status: 'failed', error: closeError };
  }

  private async abortAndReset(
    closeError: unknown
  ): Promise<Extract<ApplicationCloseOutcome, { status: 'lifecycle_unknown' }> | null> {
    try {
      await this.operations.abortNativeClose();
    } catch (abortError) {
      const failure: ApplicationCloseAbortFailure = {
        code: 'application_close_abort_failed',
        message: 'Loom could not leave its native closing state.',
        retryable: false,
        close_error: closeError,
        abort_error: abortError
      };
      this.operations.fail(failure);
      return { status: 'lifecycle_unknown', error: failure };
    }
    this.operations.reset();
    return null;
  }
}
