/** Cumulative counters from one getStats() report, stamped with the report's own clock (ms). */
export interface StatsSnapshot {
  at: number
  bytesReceived: number
  packetsReceived: number
  packetsLost: number
  framesDecoded: number
  framesDropped: number
  freezeCount: number
  nackCount: number
  pliCount: number
  keyFramesDecoded: number
  /** Seconds; the browser already smooths RTP interarrival jitter. */
  jitter: number
  jitterBufferDelay: number
  jitterBufferEmittedCount: number
  totalProcessingDelay: number
  totalDecodeTime: number
  /** ICE consent checks on the selected pair; Chrome sends one every ~2.5 s. */
  totalRoundTripTime: number
  responsesReceived: number
  currentRoundTripTime: number
}

export interface ConnectionSample {
  at: number
  fps: number
  bitrateMbps: number
  rttMs: number
  jitterMs: number
  /** Loss over the window, in percent of expected packets. */
  lossPct: number
  lostPackets: number
  droppedFrames: number
  jitterBufferMs: number
  decodeMs: number
  /** First packet received to frame decoded, per frame. */
  processingMs: number
  /** 'channel' is a data-channel ping on the video's path; 'ice' is the browser's consent checks. */
  rttSource: 'ice' | 'channel'
  /** Present only while a stats view runs the presented-frame callback. */
  presentedFps?: number
  maxFrameGapMs?: number
  stutters?: number
  receiveToPresentMs?: number
}

export interface StatsRoute {
  localCandidateId: string
  remoteCandidateId: string
}

export interface StatsReading {
  snapshot: StatsSnapshot | null
  frameWidth: number
  frameHeight: number
  framesPerSecond: number
  route: StatsRoute | null
}

export type StatsLevel = 'ok' | 'warning' | 'bad'
export type HudCorner = 'top-left' | 'top-right' | 'bottom-left' | 'bottom-right'

export const HUD_CORNERS: HudCorner[] = ['top-left', 'top-right', 'bottom-left', 'bottom-right']

type Stat = Record<string, any>

const num = (value: unknown) => (typeof value === 'number' && Number.isFinite(value) ? value : 0)

/** Pulls the inbound video counters and the selected candidate pair out of one report. */
export function readStatsReport(stats: Iterable<Stat>): StatsReading {
  const all = [...stats]
  const video = all.find(s => s.type === 'inbound-rtp' && s.kind === 'video')
  const selectedId = all.find(s => s.type === 'transport' && s.selectedCandidatePairId)?.selectedCandidatePairId
  const pairs = all.filter(s => s.type === 'candidate-pair')
  const pair = pairs.find(s => s.id === selectedId)
    ?? pairs.find(s => s.nominated && s.state === 'succeeded')
    ?? pairs.find(s => s.state === 'succeeded' && (s.bytesReceived > 0 || s.bytesSent > 0))
  return {
    snapshot: video ? {
      at: num(video.timestamp),
      bytesReceived: num(video.bytesReceived),
      packetsReceived: num(video.packetsReceived),
      packetsLost: num(video.packetsLost),
      framesDecoded: num(video.framesDecoded),
      framesDropped: num(video.framesDropped),
      freezeCount: num(video.freezeCount),
      nackCount: num(video.nackCount),
      pliCount: num(video.pliCount),
      keyFramesDecoded: num(video.keyFramesDecoded),
      jitter: num(video.jitter),
      jitterBufferDelay: num(video.jitterBufferDelay),
      jitterBufferEmittedCount: num(video.jitterBufferEmittedCount),
      totalProcessingDelay: num(video.totalProcessingDelay),
      totalDecodeTime: num(video.totalDecodeTime),
      totalRoundTripTime: num(pair?.totalRoundTripTime),
      responsesReceived: num(pair?.responsesReceived),
      currentRoundTripTime: num(pair?.currentRoundTripTime),
    } : null,
    frameWidth: num(video?.frameWidth),
    frameHeight: num(video?.frameHeight),
    framesPerSecond: num(video?.framesPerSecond),
    route: pair ? { localCandidateId: pair.localCandidateId ?? '', remoteCandidateId: pair.remoteCandidateId ?? '' } : null,
  }
}

/** Rates are taken over a trailing window so a 2 Hz refresh does not show frame-count quantisation. */
export const STATS_WINDOW_MS = 1000

/**
 * Turns cumulative snapshots into rates over the trailing window. Returns null until a baseline
 * exists; a counter that goes backwards means a new peer connection and restarts the baseline.
 */
export function createConnectionSampler(windowMs = STATS_WINDOW_MS) {
  let history: StatsSnapshot[] = []
  let rttMs = 0
  return (snap: StatsSnapshot): ConnectionSample | null => {
    const last = history[history.length - 1]
    if (last && snap.at <= last.at) return null
    if (last && (snap.bytesReceived < last.bytesReceived || snap.packetsReceived < last.packetsReceived
      || snap.framesDecoded < last.framesDecoded || snap.responsesReceived < last.responsesReceived)) {
      history = []
      rttMs = 0
    }
    history.push(snap)
    // Keep the newest snapshot that is still roughly a window old; timers drift by a few ms.
    while (history.length > 2 && snap.at - history[1]!.at >= windowMs * 0.9) history.shift()
    const base = history[0]!
    if (base === snap) {
      rttMs = snap.currentRoundTripTime * 1000
      return null
    }
    const seconds = (snap.at - base.at) / 1000
    const frames = snap.framesDecoded - base.framesDecoded
    const received = snap.packetsReceived - base.packetsReceived
    const lost = Math.max(0, snap.packetsLost - base.packetsLost)
    const emitted = snap.jitterBufferEmittedCount - base.jitterBufferEmittedCount
    const responses = snap.responsesReceived - base.responsesReceived
    // Consent checks are sparse, so hold the last measured round trip between them.
    if (responses > 0) rttMs = ((snap.totalRoundTripTime - base.totalRoundTripTime) / responses) * 1000
    else if (!rttMs) rttMs = snap.currentRoundTripTime * 1000
    const perFrame = (total: number) => (frames > 0 ? (total / frames) * 1000 : 0)
    return {
      at: snap.at,
      fps: frames / seconds,
      bitrateMbps: ((snap.bytesReceived - base.bytesReceived) * 8) / (seconds * 1e6),
      rttMs,
      jitterMs: snap.jitter * 1000,
      lossPct: lost + received > 0 ? (lost / (lost + received)) * 100 : 0,
      lostPackets: lost,
      droppedFrames: Math.max(0, snap.framesDropped - base.framesDropped),
      jitterBufferMs: emitted > 0 ? ((snap.jitterBufferDelay - base.jitterBufferDelay) / emitted) * 1000 : 0,
      decodeMs: perFrame(snap.totalDecodeTime - base.totalDecodeTime),
      processingMs: perFrame(snap.totalProcessingDelay - base.totalProcessingDelay),
      rttSource: 'ice',
    }
  }
}

/** Sub-10 ms values keep a decimal so a LAN round trip does not read as 0 or 1 ms. */
export function formatMs(ms: number): string {
  if (!Number.isFinite(ms) || ms <= 0) return '0'
  if (ms < 1) return '<1'
  return ms < 10 ? ms.toFixed(1) : String(Math.round(ms))
}

export function formatMbps(mbps: number): string {
  if (!Number.isFinite(mbps) || mbps <= 0) return '0'
  return mbps < 10 ? mbps.toFixed(2) : mbps.toFixed(1)
}

export function formatPct(pct: number): string {
  if (!Number.isFinite(pct) || pct <= 0) return '0'
  return pct < 0.1 ? '<0.1' : pct.toFixed(1)
}

type NumericSampleKey = { [K in keyof ConnectionSample]-?: ConnectionSample[K] extends number | undefined ? K : never }[keyof ConnectionSample]

/** Numeric series for charts; fields that need a running stats view read as 0 before it starts. */
export function sampleSeries(samples: ConnectionSample[], key: NumericSampleKey): number[] {
  return samples.map(s => s[key] ?? 0)
}

/** What the viewer sees: presented frames when measured, otherwise decoded frames. */
export function shownFps(s: ConnectionSample | undefined): number {
  return s?.presentedFps ?? s?.fps ?? 0
}

/** Browser-side delay: receive to presentation when measured, otherwise first packet to decoded. */
export function browserDelayMs(s: ConnectionSample | undefined): number {
  return s?.receiveToPresentMs || s?.processingMs || 0
}

/** Gaps only matter on a stream that is normally fluid; a static desktop may legitimately idle. */
export function frameGapLevel(maxGapMs: number, recentPeakFps: number): StatsLevel {
  if (recentPeakFps < 20) return 'ok'
  return maxGapMs > 200 ? 'bad' : maxGapMs > 50 ? 'warning' : 'ok'
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
