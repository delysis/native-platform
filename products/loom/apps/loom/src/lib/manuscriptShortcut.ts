/** Own the manuscript command before the editor handles Return itself. */
export function installManuscriptRunShortcut(run: () => void): () => void {
  const handle = (event: KeyboardEvent): void => {
    if (event.defaultPrevented || event.isComposing || event.keyCode === 229 ||
        event.key !== 'Enter' || !(event.metaKey || event.ctrlKey) ||
        event.shiftKey || event.altKey || !(event.target instanceof Element) ||
        !event.target.closest('.editor-stage')) return;
    event.preventDefault();
    event.stopPropagation();
    run();
  };
  window.addEventListener('keydown', handle, true);
  return () => window.removeEventListener('keydown', handle, true);
}
