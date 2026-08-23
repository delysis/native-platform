import type { ProjectCloseOutcome } from './applicationCloseCoordinator';
import { closeResultMayHaveCommitted, failureIsDefiniteContention } from './sessionSafety';
import type { LoomFailure } from './types';

export interface DetachedProjectIdentity {
  project_id: string;
  session_id: string;
}

export interface DetachedProjectCloseReceipt extends DetachedProjectIdentity {
  command_id: string;
}

export interface DetachedProjectCloseOperations {
  currentProject: () => Promise<DetachedProjectIdentity>;
  disableAutomation: (projectId: string, sessionId: string) => Promise<unknown>;
  closeProject: (
    projectId: string,
    sessionId: string,
    commandId: string
  ) => Promise<DetachedProjectCloseReceipt>;
  newCommandId: () => string;
  normalizeFailure: (error: unknown) => LoomFailure;
}

interface DetachedProjectCloseCapture extends DetachedProjectIdentity {
  command_id: string;
  automation_disabled: boolean;
}

function projectIsNotOpen(failure: LoomFailure): boolean {
  return failure.code === 'project_not_open';
}

function shouldRetryExactClose(failure: LoomFailure): boolean {
  return failureIsDefiniteContention(failure) || closeResultMayHaveCommitted(failure);
}

/**
 * Owns the identity and command ID for a native project that has not yet been
 * attached to Svelte. A bounded native close may need several renderer turns;
 * every retry reuses this exact capture, including after a lost reply.
 */
export class DetachedProjectCloseCoordinator {
  private capture: DetachedProjectCloseCapture | null = null;

  constructor(private readonly operations: DetachedProjectCloseOperations) {}

  reset(): void {
    this.capture = null;
  }

  async close(): Promise<ProjectCloseOutcome> {
    if (!this.capture) {
      let current: DetachedProjectIdentity;
      try {
        current = await this.operations.currentProject();
      } catch (error) {
        const failure = this.operations.normalizeFailure(error);
        if (projectIsNotOpen(failure)) return { status: 'closed' };
        // This read cannot have mutated native state. A transport-uncertain or
        // retryable reply means presence is unknown, so let the bounded close
        // scheduler re-read rather than aborting Quit or guessing absence.
        if (closeResultMayHaveCommitted(failure)) return { status: 'quiesced' };
        throw error;
      }
      this.capture = {
        ...current,
        command_id: this.operations.newCommandId(),
        automation_disabled: false
      };
    }

    const capture = this.capture;
    if (!capture.automation_disabled) {
      try {
        await this.operations.disableAutomation(capture.project_id, capture.session_id);
        capture.automation_disabled = true;
      } catch (error) {
        const failure = this.operations.normalizeFailure(error);
        if (projectIsNotOpen(failure)) {
          this.capture = null;
          return { status: 'closed' };
        }
        if (failure.retryable || closeResultMayHaveCommitted(failure)) {
          return { status: 'quiesced' };
        }
        throw error;
      }
    }

    let receipt: DetachedProjectCloseReceipt;
    try {
      receipt = await this.operations.closeProject(
        capture.project_id,
        capture.session_id,
        capture.command_id
      );
    } catch (error) {
      const failure = this.operations.normalizeFailure(error);
      if (projectIsNotOpen(failure)) {
        this.capture = null;
        return { status: 'closed' };
      }
      if (shouldRetryExactClose(failure)) return { status: 'quiesced' };
      throw error;
    }
    if (
      receipt.command_id !== capture.command_id ||
      receipt.project_id !== capture.project_id ||
      receipt.session_id !== capture.session_id
    ) throw new Error('The desktop returned a detached close receipt for another project session.');

    this.capture = null;
    return { status: 'closed' };
  }
}
