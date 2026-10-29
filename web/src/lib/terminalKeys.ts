export type TerminalModifiers = { ctrl: boolean; alt: boolean }

export function toggleTerminalModifier(state: TerminalModifiers, key: 'Ctrl' | 'Alt'): TerminalModifiers {
  return key === 'Ctrl' ? { ...state, ctrl: !state.ctrl } : { ...state, alt: !state.alt }
}

export function consumeTerminalKey(state: TerminalModifiers, key: string) {
  const sequences: Record<string, string> = {
    Esc: '\x1b', Escape: '\x1b', Tab: '\t', Enter: '\r', Backspace: '\x7f',
    '↑': '\x1b[A', ArrowUp: '\x1b[A', '↓': '\x1b[B', ArrowDown: '\x1b[B',
    '→': '\x1b[C', ArrowRight: '\x1b[C', '←': '\x1b[D', ArrowLeft: '\x1b[D',
  }
  let bytes = sequences[key] ?? (key.length === 1 ? key : '')
  const arrow = /^\x1b\[([ABCD])$/.exec(bytes)
  if (arrow && (state.ctrl || state.alt)) {
    bytes = `\x1b[1;${1 + (state.alt ? 2 : 0) + (state.ctrl ? 4 : 0)}${arrow[1]}`
    return { bytes, modifiers: { ctrl: false, alt: false } }
  }
  if (state.ctrl) {
    if (key === ' ' || key === '@' || key === '2') bytes = '\0'
    else if (key === '?') bytes = '\x7f'
    else if (key.length === 1 && key.toUpperCase().charCodeAt(0) >= 64 && key.toUpperCase().charCodeAt(0) <= 95) bytes = String.fromCharCode(key.toUpperCase().charCodeAt(0) & 31)
  }
  if (state.alt) bytes = `\x1b${bytes}`
  return { bytes, modifiers: { ctrl: false, alt: false } }
}
