/**
 * Round trip over the HID data channel, which shares the video's ICE path. The board echoes the
 * payload with MSG_PONG; HID parsing never sees it, and older builds just ignore the type.
 */
export const MSG_PING = 0x10
export const MSG_PONG = 0x11
const PING_BYTES = 13
/** A ping without a pong after this long is counted lost and a new one may go out. */
export const PING_TIMEOUT_MS = 2000

export function encodePing(seq: number, sentAt: number): ArrayBuffer {
  const buffer = new ArrayBuffer(PING_BYTES)
  const view = new DataView(buffer)
  view.setUint8(0, MSG_PING)
  view.setUint32(1, seq, true)
  view.setFloat64(5, sentAt, true)
  return buffer
}

export function decodePong(data: ArrayBuffer): { seq: number; sentAt: number } | null {
  if (data.byteLength !== PING_BYTES) return null
  const view = new DataView(data)
  if (view.getUint8(0) !== MSG_PONG) return null
  return { seq: view.getUint32(1, true), sentAt: view.getFloat64(5, true) }
}

/** One ping in flight at a time; round trips are averaged over the trailing window. */
export function createPingTracker(windowMs = 1000) {
  let seq = 0
  let pending: { seq: number; sentAt: number } | null = null
  let results: { at: number; ms: number }[] = []
  return {
    next(now: number): ArrayBuffer | null {
      if (pending && now - pending.sentAt < PING_TIMEOUT_MS) return null
      seq = (seq + 1) >>> 0
      pending = { seq, sentAt: now }
      return encodePing(seq, now)
    },
    receive(data: ArrayBuffer, now: number): boolean {
      const pong = decodePong(data)
      if (!pong) return false
      if (pending && pong.seq === pending.seq && pong.sentAt === pending.sentAt) {
        results.push({ at: now, ms: now - pong.sentAt })
        pending = null
      }
      return true
    },
    /** Mean round trip of pongs inside the window, or null when none arrived. */
    average(now: number): number | null {
      results = results.filter(r => now - r.at <= windowMs)
      return results.length ? results.reduce((sum, r) => sum + r.ms, 0) / results.length : null
    },
    reset() {
      pending = null
      results = []
    },
  }
}
