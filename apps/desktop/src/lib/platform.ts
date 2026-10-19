/** Platform conventions shared by the desktop shell and its controls. */
const platform = typeof navigator === "undefined" ? "" : navigator.platform;

export const isMacOS = /Mac|iPhone|iPad/i.test(platform);

/** Format a shortcut with the modifier users expect on this operating system. */
export function shortcut(keys: string): string {
  if (!isMacOS) return keys;
  return keys
    .replace(/^Ctrl\+Meta\+/, "⌃⌘")
    .replace(/^Ctrl\+/, "⌘")
    .replaceAll("Shift+", "⇧")
    .replaceAll("Alt+", "⌥");
}

export function hasPrimaryModifier(event: KeyboardEvent): boolean {
  return isMacOS ? event.metaKey : event.ctrlKey;
}
