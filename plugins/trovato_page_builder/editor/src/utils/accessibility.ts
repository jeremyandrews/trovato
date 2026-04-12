/**
 * Page builder editor accessibility utilities.
 *
 * Provides keyboard navigation, screen reader announcements, and focus
 * management for the Puck visual editor. Supplements drag-and-drop with
 * keyboard-accessible alternatives per Trovato's "Keyboard Navigation as
 * Primary Input" foundational principle.
 */

// ---------------------------------------------------------------------------
// Screen reader announcements via aria-live region
// ---------------------------------------------------------------------------

let announceEl: HTMLElement | null = null;

/**
 * Initialize the aria-live announcement region.
 * Call once when the page builder mounts.
 */
export function initAnnouncer(): void {
  if (announceEl) return;
  announceEl = document.createElement("div");
  announceEl.setAttribute("aria-live", "polite");
  announceEl.setAttribute("aria-atomic", "true");
  announceEl.setAttribute("role", "status");
  announceEl.className = "sr-only";
  // Visually hidden but available to screen readers
  announceEl.style.cssText =
    "position:absolute;width:1px;height:1px;overflow:hidden;clip:rect(0,0,0,0);white-space:nowrap;border:0;";
  document.body.appendChild(announceEl);
}

/**
 * Announce a message to screen readers via the aria-live region.
 * Brief delay ensures the screen reader detects the content change.
 */
export function announce(message: string): void {
  if (!announceEl) initAnnouncer();
  if (!announceEl) return;
  announceEl.textContent = "";
  setTimeout(() => {
    announceEl!.textContent = message;
  }, 50);
}

// ---------------------------------------------------------------------------
// Focus management
// ---------------------------------------------------------------------------

/**
 * Focus a component by its index in the editor canvas.
 * Uses data-component-index attributes set by the component wrapper.
 */
export function focusComponent(index: number): void {
  const el = document.querySelector<HTMLElement>(
    `[data-component-index="${index}"]`
  );
  el?.focus();
}

/**
 * Focus the first focusable element inside a container.
 * Used when opening a settings panel.
 */
export function focusFirstInput(container: HTMLElement): void {
  const focusable = container.querySelector<HTMLElement>(
    'input, select, textarea, [tabindex="0"]'
  );
  focusable?.focus();
}

// ---------------------------------------------------------------------------
// Keyboard shortcuts
// ---------------------------------------------------------------------------

export interface KeyboardActions {
  moveUp: (index: number) => void;
  moveDown: (index: number) => void;
  remove: (index: number) => void;
  openSettings: (index: number) => void;
}

/**
 * Handle keyboard events on a component wrapper.
 * Supports: Alt+Arrow (move), Delete (remove), Enter (settings).
 */
export function handleComponentKeyDown(
  e: KeyboardEvent,
  index: number,
  totalComponents: number,
  actions: KeyboardActions
): void {
  if (e.altKey && e.key === "ArrowUp" && index > 0) {
    e.preventDefault();
    actions.moveUp(index);
    announce(`Component moved up to position ${index}`);
    // Focus will be set by the caller after state update
  } else if (e.altKey && e.key === "ArrowDown" && index < totalComponents - 1) {
    e.preventDefault();
    actions.moveDown(index);
    announce(`Component moved down to position ${index + 2}`);
  } else if (e.key === "Delete" || e.key === "Backspace") {
    e.preventDefault();
    actions.remove(index);
    announce("Component removed");
  } else if (e.key === "Enter" && !e.altKey && !e.ctrlKey && !e.metaKey) {
    e.preventDefault();
    actions.openSettings(index);
  }
}

// ---------------------------------------------------------------------------
// Keyboard shortcut reference
// ---------------------------------------------------------------------------

export const KEYBOARD_SHORTCUTS = [
  { keys: "Tab / Shift+Tab", action: "Navigate between components" },
  { keys: "Enter", action: "Open component settings" },
  { keys: "Escape", action: "Close settings / deselect" },
  { keys: "Alt+\u2191", action: "Move component up" },
  { keys: "Alt+\u2193", action: "Move component down" },
  { keys: "Delete", action: "Remove component" },
  { keys: "Ctrl+S / Cmd+S", action: "Save page" },
  { keys: "? or F1", action: "Show keyboard shortcuts" },
] as const;
