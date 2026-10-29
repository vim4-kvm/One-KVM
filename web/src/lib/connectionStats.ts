export interface RawConnectionStats {
  bytesReceived: number
  packetsReceived: number
  packetsLost: number
  framesPerSecond: number
  jitter: number
  roundTripTime: number
}

export interface ConnectionSample {
  fps: number
  bitrateMbps: number
  rttMs: number
  jitterMs: number
  /** Loss over the last sampling interval, in percent of expected packets. */
  lossPct: number
}

export type StatsLevel = 'ok' | 'warning' | 'bad'
export type HudCorner = 'top-left' | 'top-right' | 'bottom-left' | 'bottom-right'

export const HUD_CORNERS: HudCorner[] = ['top-left', 'top-right', 'bottom-left', 'bottom-right']

/** Turns cumulative WebRTC counters into per-interval rates. */
export function createConnectionSampler() {
  let previous: { at: number; bytes: number; received: number; lost: number } | null = null
  return (raw: RawConnectionStats, at: number): ConnectionSample => {
    const prev = previous
    previous = { at, bytes: raw.bytesReceived, received: raw.packetsReceived, lost: raw.packetsLost }
    // Counters restart with a new peer connection; treat a decrease as a fresh baseline.
    const valid = prev && at > prev.at && raw.bytesReceived >= prev.bytes && raw.packetsReceived >= prev.received
    const seconds = valid ? (at - prev.at) / 1000 : 0
    const lost = valid ? Math.max(0, raw.packetsLost - prev.lost) : 0
    const received = valid ? raw.packetsReceived - prev.received : 0
    return {
      fps: raw.framesPerSecond || 0,
      bitrateMbps: valid ? ((raw.bytesReceived - prev.bytes) * 8) / (seconds * 1e6) : 0,
      rttMs: (raw.roundTripTime || 0) * 1000,
      jitterMs: (raw.jitter || 0) * 1000,
      lossPct: lost + received > 0 ? (lost / (lost + received)) * 100 : 0,
    }
  }
}

export function lossLevel(pct: number): StatsLevel {
  return pct > 5 ? 'bad' : pct > 1 ? 'warning' : 'ok'
}

export function rttLevel(ms: number): StatsLevel {
  return ms > 250 ? 'bad' : ms > 100 ? 'warning' : 'ok'
}

/** Static desktops legitimately lower the frame rate, so only a sharp drop from the recent peak warns. */
export function fpsLevel(fps: number, recentPeak: number): StatsLevel {
  return recentPeak >= 10 && fps < recentPeak * 0.5 ? 'warning' : 'ok'
}

/** Picks the workspace corner nearest to the card's centre. */
export function nearestCorner(centreX: number, centreY: number, width: number, height: number): HudCorner {
  const vertical = centreY < height / 2 ? 'top' : 'bottom'
  const horizontal = centreX < width / 2 ? 'left' : 'right'
  return `${vertical}-${horizontal}`
}

/** SVG polyline points for a sparkline; the scale starts at zero so flat traffic does not look noisy. */
export function sparklinePoints(values: number[], width: number, height: number): string {
  if (values.length < 2) return ''
  const max = Math.max(...values) || 1
  const step = width / (values.length - 1)
  return values
    .map((value, index) => `${(index * step).toFixed(1)},${(height - (value / max) * (height - 1) - 0.5).toFixed(1)}`)
    .join(' ')
}
