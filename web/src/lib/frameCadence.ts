/** Presented-frame cadence from requestVideoFrameCallback, summarised over a trailing window. */
export interface FrameCadenceSummary {
  /** Frames the compositor actually presented, per second. */
  presentedFps: number
  /** Longest presentation interval per frame; a stall shows here even when the average fps looks fine. */
  maxFrameGapMs: number
  /** Presentation intervals above STUTTER_GAP_MS. */
  stutters: number
  /** Last packet received to frame presented; 0 when the browser does not report receiveTime. */
  receiveToPresentMs: number
}

export const STUTTER_GAP_MS = 50

interface FrameEntry { at: number; presented: number; delay: number | null }

export function createFrameCadence(windowMs = 1000) {
  let frames: FrameEntry[] = []
  return {
    add(meta: { presentationTime: number; presentedFrames: number; receiveTime?: number }) {
      const last = frames[frames.length - 1]
      // presentedFrames restarts when the element gets a new stream.
      if (last && (meta.presentedFrames <= last.presented || meta.presentationTime <= last.at)) frames = []
      frames.push({
        at: meta.presentationTime,
        presented: meta.presentedFrames,
        delay: meta.receiveTime ? meta.presentationTime - meta.receiveTime : null,
      })
      // Keep one entry older than the window as the baseline for the first interval.
      while (frames.length > 2 && meta.presentationTime - frames[1]!.at >= windowMs) frames.shift()
    },
    summarize(now: number): FrameCadenceSummary | null {
      const last = frames[frames.length - 1]
      // No frame for a whole window is itself the stall; report it rather than stale numbers.
      if (!last || frames.length < 2) return null
      if (now - last.at >= windowMs) return { presentedFps: 0, maxFrameGapMs: now - last.at, stutters: 1, receiveToPresentMs: 0 }
      const base = frames[0]!
      let maxGap = 0
      let stutters = 0
      let delaySum = 0
      let delays = 0
      for (let i = 1; i < frames.length; i++) {
        const prev = frames[i - 1]!
        const cur = frames[i]!
        // A busy main thread skips callbacks while frames keep presenting; spread the interval over them.
        const gap = (cur.at - prev.at) / (cur.presented - prev.presented)
        if (gap > maxGap) maxGap = gap
        if (gap > STUTTER_GAP_MS) stutters++
        if (cur.delay !== null && cur.delay >= 0) {
          delaySum += cur.delay
          delays++
        }
      }
      // An open interval long enough to be a stall counts too; shorter ones are just sampling phase.
      if (now - last.at > STUTTER_GAP_MS) maxGap = Math.max(maxGap, now - last.at)
      return {
        presentedFps: ((last.presented - base.presented) * 1000) / (last.at - base.at),
        maxFrameGapMs: maxGap,
        stutters,
        receiveToPresentMs: delays ? delaySum / delays : 0,
      }
    },
    reset() {
      frames = []
    },
  }
}
