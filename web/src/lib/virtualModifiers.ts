export type ModifierLatch = 'armed' | 'locked'

/** A second tap within this window locks a modifier instead of releasing it. */
export const DOUBLE_TAP_MS = 400

/**
 * Sticky-key behaviour for on-screen modifiers: one tap arms it for the next key,
 * a quick second tap locks it, any other tap releases it.
 */
export function tapModifier(current: ModifierLatch | undefined, sinceLastTapMs: number): ModifierLatch | undefined {
  if (!current) return 'armed'
  if (current === 'armed' && sinceLastTapMs <= DOUBLE_TAP_MS) return 'locked'
  return undefined
}

/** Armed modifiers are consumed by the next non-modifier key; locked ones stay. */
export function consumeArmed<K>(latches: Map<K, ModifierLatch>): { kept: Map<K, ModifierLatch>; released: K[] } {
  const kept = new Map<K, ModifierLatch>()
  const released: K[] = []
  for (const [key, latch] of latches) {
    if (latch === 'locked') kept.set(key, latch)
    else released.push(key)
  }
  return { kept, released }
}

/** Human-readable combo prefix, e.g. "Ctrl + Shift", with left/right duplicates collapsed. */
export function comboLabel(labels: string[]): string {
  return [...new Set(labels)].join(' + ')
}
