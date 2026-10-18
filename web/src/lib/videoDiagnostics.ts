// Opt-in client-side measurements. No pixels, credentials or ICE addresses are
// collected. Callback timestamps describe compositor submissions, not photons.
export function startVideoDiagnostics(peer: RTCPeerConnection): () => void {
  const started = performance.now()
  const samples: unknown[] = []
  const gaps: number[] = []
  const animationGaps: number[] = []
  const receiveToPresentation: number[] = []
  let previous: RTCInboundRtpStreamStats | undefined
  let video: HTMLVideoElement | undefined
  let lastFrame: VideoFrameCallbackMetadata | undefined
  let lastAnimation: number | undefined
  let missedCallbacks = 0
  let hidden = document.hidden
  let active = true
  let timer = 0
  let frameCallback = 0
  let animationCallback = 0

  const distribution = (values: number[]) => {
    const sorted = [...values].sort((a, b) => a - b)
    const percentile = (fraction: number) => sorted.length
      ? sorted[Math.floor((sorted.length - 1) * fraction)] : null
    return { count: sorted.length, p50: percentile(.5), p99: percentile(.99),
      max: percentile(1), over50: sorted.filter(value => value > 50).length }
  }

  function stop() {
    active = false
    clearTimeout(timer)
    if (video) video.cancelVideoFrameCallback(frameCallback)
    cancelAnimationFrame(animationCallback)
  }

  function finish(reason: string) {
    stop()
    // Serialize now: expanding a live console object later must not change the evidence.
    console.info('[VideoCadence] ' + JSON.stringify({
      reason, userAgent: navigator.userAgent, elapsedMs: performance.now() - started,
      hiddenDuringSample: hidden, missedCallbacks,
      presentationMs: distribution(gaps), animationMs: distribution(animationGaps),
      receiveToPresentationMs: distribution(receiveToPresentation),
      noVideoCallbackTailMs: lastFrame ? performance.now() - lastFrame.presentationTime : null,
      samples,
    }))
  }

  function onFrame(_now: number, frame: VideoFrameCallbackMetadata) {
    if (!active) return
    if (lastFrame) {
      if (frame.presentedFrames < lastFrame.presentedFrames) {
        finish('video-counter-reset')
        return
      }
      gaps.push(frame.presentationTime - lastFrame.presentationTime)
      missedCallbacks += Math.max(0, frame.presentedFrames - lastFrame.presentedFrames - 1)
    }
    if (frame.receiveTime !== undefined) {
      receiveToPresentation.push(frame.presentationTime - frame.receiveTime)
    }
    lastFrame = frame
    frameCallback = video!.requestVideoFrameCallback(onFrame)
  }

  function onAnimation(now: number) {
    if (!active) return
    if (lastAnimation !== undefined) animationGaps.push(now - lastAnimation)
    lastAnimation = now
    hidden ||= document.hidden
    animationCallback = requestAnimationFrame(onAnimation)
  }

  async function sample() {
    try {
      const report = await peer.getStats()
      if (!active) return
      hidden ||= document.hidden
      const inbound = [...report.values()].find(stat =>
        stat.type === 'inbound-rtp' && stat.kind === 'video') as
          (RTCInboundRtpStreamStats & { powerEfficientDecoder?: boolean }) | undefined
      if (inbound) {
        if (previous && (previous.id !== inbound.id || previous.ssrc !== inbound.ssrc ||
          previous.codecId !== inbound.codecId)) {
          finish('receive-stream-changed')
          return
        }
        const delta = (key: keyof RTCInboundRtpStreamStats) => {
          const before = previous?.[key], after = inbound[key]
          return typeof before === 'number' && typeof after === 'number' ? after - before : NaN
        }
        const decoded = delta('framesDecoded')
        const emitted = delta('jitterBufferEmittedCount')
        const perFrame = (key: keyof RTCInboundRtpStreamStats, count: number) =>
          count > 0 ? 1000 * delta(key) / count : null
        const elapsed = previous ? inbound.timestamp - previous.timestamp : 0
        samples.push({
          elapsedMs: performance.now() - started,
          codec: inbound.codecId ? report.get(inbound.codecId)?.mimeType : undefined,
          decoder: inbound.decoderImplementation,
          powerEfficientDecoder: inbound.powerEfficientDecoder,
          width: inbound.frameWidth, height: inbound.frameHeight,
          fps: elapsed > 0 ? decoded * 1000 / elapsed : null,
          mbps: elapsed > 0 ? delta('bytesReceived') * .008 / elapsed : null,
          decodeMs: perFrame('totalDecodeTime', decoded),
          processingMs: perFrame('totalProcessingDelay', decoded),
          bufferMs: perFrame('jitterBufferDelay', emitted),
          targetBufferMs: perFrame('jitterBufferTargetDelay', emitted),
          minimumBufferMs: perFrame('jitterBufferMinimumDelay', emitted),
          dropped: delta('framesDropped'), lost: delta('packetsLost'),
          freezes: delta('freezeCount'), keyFrames: delta('keyFramesDecoded'),
          pli: delta('pliCount'), nack: delta('nackCount'),
          jitterMs: inbound.jitter === undefined ? null : inbound.jitter * 1000,
        })
        previous = inbound
      }
      if (!video) {
        const tracks = peer.getReceivers().map(receiver => receiver.track)
        video = [...document.querySelectorAll('video')].find(element =>
          element.srcObject instanceof MediaStream &&
          element.srcObject.getVideoTracks().some(track => tracks.includes(track)))
        if (video) frameCallback = video.requestVideoFrameCallback(onFrame)
      }
      if (performance.now() - started >= 30000) {
        finish('complete')
      } else {
        timer = window.setTimeout(sample, 1000)
      }
    } catch (error) {
      if (active) finish(error instanceof Error ? error.name : 'stats-error')
    }
  }

  console.info('[VideoCadence] Collecting 30 seconds; keep the video visible and moving.')
  animationCallback = requestAnimationFrame(onAnimation)
  void sample()
  return stop
}
