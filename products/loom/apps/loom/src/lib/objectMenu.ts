/** A single contextual action, absent from the document and normal tab order. */
export function objectMenu(dom: HTMLElement, label: () => string, action: () => void, focus: () => void): () => void {
  let menu: HTMLDivElement | undefined;
  function close(): void {
    menu?.remove();
    menu = undefined;
    document.removeEventListener('pointerdown', outside, true);
    document.removeEventListener('keydown', key, true);
  }
  function outside(event: PointerEvent): void {
    if (!menu?.contains(event.target as Node)) close();
  }
  function key(event: KeyboardEvent): void {
    if (event.key !== 'Escape' && event.key !== 'Tab') return;
    event.preventDefault();
    close();
    focus();
  }
  function open(event: MouseEvent): void {
    event.preventDefault();
    event.stopPropagation();
    close();
    menu = document.createElement('div');
    menu.className = 'loom-object-menu';
    menu.contentEditable = 'false';
    menu.setAttribute('role', 'menu');
    const button = document.createElement('button');
    button.type = 'button';
    button.setAttribute('role', 'menuitem');
    button.textContent = label();
    button.addEventListener('click', () => { close(); action(); });
    menu.append(button);
    document.body.append(menu);
    menu.style.left = `${Math.max(0, Math.min(event.clientX, innerWidth - menu.offsetWidth))}px`;
    menu.style.top = `${Math.max(0, Math.min(event.clientY, innerHeight - menu.offsetHeight))}px`;
    document.addEventListener('pointerdown', outside, true);
    document.addEventListener('keydown', key, true);
    button.focus();
  }
  dom.addEventListener('contextmenu', open);
  return () => { close(); dom.removeEventListener('contextmenu', open); };
}
