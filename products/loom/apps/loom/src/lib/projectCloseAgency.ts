export interface ProjectCloseAgencySnapshot {
  readonly suggestionsEnabled: boolean;
  readonly focusMode: false;
}

export interface ProjectCloseAgencyRestoration {
  setFocusMode: (enabled: false) => Promise<void>;
  setSuggestionsEnabled: (enabled: boolean) => Promise<void>;
}

export function captureProjectCloseAgency(
  suggestionsEnabled: boolean
): ProjectCloseAgencySnapshot {
  return { suggestionsEnabled, focusMode: false };
}

/** Restore the authoring gate before automatic admission can resume. */
export async function restoreProjectCloseAgency(
  snapshot: ProjectCloseAgencySnapshot,
  restoration: ProjectCloseAgencyRestoration
): Promise<void> {
  await restoration.setFocusMode(snapshot.focusMode);
  await restoration.setSuggestionsEnabled(snapshot.suggestionsEnabled);
}
