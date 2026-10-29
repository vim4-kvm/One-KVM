export type ShortcutOs = 'windows' | 'mac' | 'android'

/** A single chord: modifiers first, then at most one main key (KeyboardEvent.code names). */
export interface Shortcut {
  id: string
  keys: string[]
  /** Optional user-given name; the chord itself is shown when absent. */
  name?: string
}

export const MAX_CUSTOM_SHORTCUTS = 12
export const STORAGE_KEY = 'vkbShortcuts'

const MODIFIER_ORDER = ['ControlLeft', 'ControlRight', 'AltLeft', 'AltRight', 'ShiftLeft', 'ShiftRight', 'MetaLeft', 'MetaRight']

export function isModifierKey(key: string): boolean {
  return MODIFIER_ORDER.includes(key)
}

const preset = (id: string, ...keys: string[]): Shortcut => ({ id: `preset:${id}`, keys })

export const PRESET_SHORTCUTS: Record<ShortcutOs, Shortcut[]> = {
  windows: [
    preset('ctrl-alt-del', 'ControlLeft', 'AltLeft', 'Delete'),
    preset('win-r', 'MetaLeft', 'KeyR'),
    preset('task-manager', 'ControlLeft', 'ShiftLeft', 'Escape'),
    preset('alt-tab', 'AltLeft', 'Tab'),
    preset('alt-f4', 'AltLeft', 'F4'),
    preset('win-l', 'MetaLeft', 'KeyL'),
  ],
  mac: [
    preset('spotlight', 'MetaLeft', 'Space'),
    preset('app-switcher', 'MetaLeft', 'Tab'),
    preset('force-quit', 'AltLeft', 'MetaLeft', 'Escape'),
    preset('quit', 'MetaLeft', 'KeyQ'),
    preset('lock', 'ControlLeft', 'MetaLeft', 'KeyQ'),
  ],
  android: [
    preset('alt-tab', 'AltLeft', 'Tab'),
    preset('ctrl-alt-del', 'ControlLeft', 'AltLeft', 'Delete'),
    preset('ctrl-alt-bksp', 'ControlLeft', 'AltLeft', 'Backspace'),
    preset('alt-meta-esc', 'AltLeft', 'MetaLeft', 'Escape'),
  ],
}

const MAC_SYMBOLS: Record<string, string> = { Control: '⌃', Alt: '⌥', Shift: '⇧', Meta: '⌘' }
const KEY_NAMES: Record<string, string> = {
  Escape: 'Esc', Delete: 'Del', Backspace: 'Bksp', Space: 'Space', Enter: 'Enter', Tab: 'Tab',
  ArrowUp: '↑', ArrowDown: '↓', ArrowLeft: '←', ArrowRight: '→', PageUp: 'PgUp', PageDown: 'PgDn',
  Insert: 'Ins', PrintScreen: 'PrtSc', ScrollLock: 'ScrLk', Backquote: '`', Minus: '-', Equal: '=',
  BracketLeft: '[', BracketRight: ']', Backslash: '\\', Semicolon: ';', Quote: "'", Comma: ',',
  Period: '.', Slash: '/', ContextMenu: 'Menu', CapsLock: 'Caps',
}

function keyName(key: string, os: ShortcutOs): string {
  const modifier = key.replace(/(Left|Right)$/, '')
  if (isModifierKey(key)) {
    if (os === 'mac') return MAC_SYMBOLS[modifier] ?? modifier
    return modifier === 'Control' ? 'Ctrl' : modifier === 'Meta' ? (os === 'windows' ? 'Win' : 'Meta') : modifier
  }
  return KEY_NAMES[key] ?? key.replace(/^Key|^Digit|^Numpad/, '')
}

/** "Ctrl+Shift+Esc" on Windows/Android, "⌥⌘Esc" on macOS. */
export function chordLabel(keys: string[], os: ShortcutOs): string {
  const names = sortChord(keys).map(key => keyName(key, os))
  if (os !== 'mac') return names.join('+')
  const mods = sortChord(keys).filter(isModifierKey).map(key => keyName(key, os)).join('')
  const main = names.slice(mods.length)
  return mods + main.join('+')
}

/** Modifiers in a stable platform order, then the main key. */
export function sortChord(keys: string[]): string[] {
  const unique = [...new Set(keys)]
  const mods = unique.filter(isModifierKey).sort((a, b) => MODIFIER_ORDER.indexOf(a) - MODIFIER_ORDER.indexOf(b))
  return [...mods, ...unique.filter(key => !isModifierKey(key))]
}

/** Folds one key press into a chord being captured. Returns whether the chord is complete. */
export function captureKey(keys: string[], key: string): { keys: string[]; complete: boolean } {
  // A finished chord is replaced by the next one rather than extended.
  if (keys.some(k => !isModifierKey(k))) keys = []
  if (isModifierKey(key)) {
    // Clicking a modifier again on the on-screen keyboard removes it.
    const next = keys.includes(key) ? keys.filter(k => k !== key) : [...keys, key]
    return { keys: sortChord(next), complete: false }
  }
  return { keys: sortChord([...keys.filter(isModifierKey), key]), complete: true }
}

/** Reads saved shortcuts, dropping anything malformed instead of failing the keyboard. */
export function parseCustomShortcuts(raw: string | null, knownKeys: (key: string) => boolean): Shortcut[] {
  let value: unknown
  try { value = JSON.parse(raw || '[]') } catch { return [] }
  if (!Array.isArray(value)) return []
  return value.flatMap((item): Shortcut[] => {
    if (!item || typeof item.id !== 'string' || !Array.isArray(item.keys)) return []
    const keys = item.keys.filter((key: unknown): key is string => typeof key === 'string' && knownKeys(key))
    if (!keys.length || keys.length !== item.keys.length || keys.filter((key: string) => !isModifierKey(key)).length > 1) return []
    const name = typeof item.name === 'string' && item.name.trim() ? item.name.trim().slice(0, 24) : undefined
    return [{ id: item.id, keys: sortChord(keys), ...(name ? { name } : {}) }]
  }).slice(0, MAX_CUSTOM_SHORTCUTS)
}

export function sameChord(a: string[], b: string[]): boolean {
  const x = sortChord(a)
  const y = sortChord(b)
  return x.length === y.length && x.every((key, i) => key === y[i])
}
