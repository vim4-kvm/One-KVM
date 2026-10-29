import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import ts from 'typescript'
import { test } from 'node:test'

const source = readFileSync(new URL('../src/lib/connectionStats.ts', import.meta.url), 'utf8')
const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext } }).outputText
const stats = await import(`data:text/javascript;base64,${Buffer.from(compiled).toString('base64')}`)

const snap = (over = {}) => ({
  at: 0, bytesReceived: 0, packetsReceived: 0, packetsLost: 0, framesDecoded: 0, framesDropped: 0,
  freezeCount: 0, nackCount: 0, pliCount: 0, keyFramesDecoded: 0, jitter: 0.002,
  jitterBufferDelay: 0, jitterBufferEmittedCount: 0, totalProcessingDelay: 0, totalDecodeTime: 0,
  totalRoundTripTime: 0, responsesReceived: 0, currentRoundTripTime: 0.004, ...over,
})

test('rates use the report clock over a trailing one-second window', () => {
  const sample = stats.createConnectionSampler()
  assert.equal(sample(snap({ at: 1000 })), null, 'the first snapshot is only a baseline')
  const half = sample(snap({ at: 1500, bytesReceived: 500_000, packetsReceived: 495, packetsLost: 5, framesDecoded: 30 }))
  assert.equal(half.fps, 60)
  assert.equal(half.bitrateMbps, 8)
  assert.equal(half.lossPct, 1)
  // At 2 Hz the window grows to a full second, then slides by one sample.
  const full = sample(snap({ at: 2000, bytesReceived: 1_000_000, packetsReceived: 990, packetsLost: 10, framesDecoded: 59 }))
  assert.equal(full.fps, 59)
  assert.equal(full.bitrateMbps, 8)
  const slid = sample(snap({ at: 2503, bytesReceived: 1_250_000, packetsReceived: 1240, packetsLost: 10, framesDecoded: 90 }))
  assert.equal(slid.at, 2503)
  assert.ok(Math.abs(slid.fps - 60 / 1.003) < 1e-9, 'baseline is the 1500 ms snapshot, not the stale one')
  assert.equal(slid.lostPackets, 5)
})

test('a timer that fires a few ms early still slides the window', () => {
  const sample = stats.createConnectionSampler()
  sample(snap({ at: 0 }))
  sample(snap({ at: 498, framesDecoded: 30 }))
  sample(snap({ at: 995, framesDecoded: 60 }))
  const next = sample(snap({ at: 1490, framesDecoded: 90 }))
  assert.equal(next.fps, 60 / 0.992)
})

test('round trip averages the consent checks inside the window and holds between them', () => {
  const sample = stats.createConnectionSampler()
  sample(snap({ at: 0, totalRoundTripTime: 0.006, responsesReceived: 6 }))
  const before = sample(snap({ at: 500, totalRoundTripTime: 0.006, responsesReceived: 6 }))
  assert.equal(before.rttMs, 4, 'falls back to the current value until a check lands')
  const check = sample(snap({ at: 1000, totalRoundTripTime: 0.009, responsesReceived: 8, currentRoundTripTime: 0.002 }))
  assert.ok(Math.abs(check.rttMs - 1.5) < 1e-9)
  const held = sample(snap({ at: 2100, totalRoundTripTime: 0.009, responsesReceived: 8, currentRoundTripTime: 0.002 }))
  assert.ok(Math.abs(held.rttMs - 1.5) < 1e-9)
})

test('receiver delays are per frame over the window', () => {
  const sample = stats.createConnectionSampler()
  sample(snap({ at: 0, framesDecoded: 100, jitterBufferDelay: 1, jitterBufferEmittedCount: 100, totalDecodeTime: 0.4, totalProcessingDelay: 0.5 }))
  const s = sample(snap({ at: 1000, framesDecoded: 160, jitterBufferDelay: 1.012, jitterBufferEmittedCount: 160, totalDecodeTime: 0.64, totalProcessingDelay: 0.77, framesDropped: 2 }))
  assert.ok(Math.abs(s.jitterBufferMs - 0.2) < 1e-9)
  assert.ok(Math.abs(s.decodeMs - 4) < 1e-9)
  assert.ok(Math.abs(s.processingMs - 4.5) < 1e-9)
  assert.equal(s.droppedFrames, 2)
  assert.equal(s.jitterMs, 2)
})

test('a counter reset from a new peer connection starts a fresh baseline instead of negative rates', () => {
  const sample = stats.createConnectionSampler()
  sample(snap({ at: 1000, bytesReceived: 5_000_000, packetsReceived: 5000, packetsLost: 50, framesDecoded: 300 }))
  assert.equal(sample(snap({ at: 2000, bytesReceived: 10_000, packetsReceived: 10, framesDecoded: 1 })), null)
  const next = sample(snap({ at: 3000, bytesReceived: 135_000, packetsReceived: 110, framesDecoded: 61 }))
  assert.equal(next.bitrateMbps, 1)
  assert.equal(next.fps, 60)
  assert.equal(sample(snap({ at: 3000 })), null, 'a repeated report timestamp is ignored')
})

test('reports pick the inbound video stream and the transport\'s selected pair', () => {
  const reading = stats.readStatsReport([
    { type: 'inbound-rtp', kind: 'audio', timestamp: 5, bytesReceived: 99 },
    { type: 'inbound-rtp', kind: 'video', timestamp: 10, bytesReceived: 1234, framesDecoded: 7, frameWidth: 3840, frameHeight: 2160, framesPerSecond: 60 },
    { type: 'candidate-pair', id: 'old', nominated: true, state: 'succeeded', totalRoundTripTime: 9, responsesReceived: 1, localCandidateId: 'La', remoteCandidateId: 'Ra' },
    { type: 'candidate-pair', id: 'sel', state: 'succeeded', totalRoundTripTime: 0.01, responsesReceived: 5, currentRoundTripTime: 0.002, localCandidateId: 'Lb', remoteCandidateId: 'Rb' },
    { type: 'transport', selectedCandidatePairId: 'sel' },
  ])
  assert.equal(reading.snapshot.at, 10)
  assert.equal(reading.snapshot.bytesReceived, 1234)
  assert.equal(reading.snapshot.jitterBufferDelay, 0, 'missing fields read as zero')
  assert.equal(reading.snapshot.responsesReceived, 5)
  assert.deepEqual(reading.route, { localCandidateId: 'Lb', remoteCandidateId: 'Rb' })
  assert.equal(reading.frameWidth, 3840)
  assert.equal(stats.readStatsReport([{ type: 'transport' }]).snapshot, null)
})

test('formatting keeps precision where small values matter', () => {
  assert.deepEqual([0, 0.4, 1, 1.46, 9.99, 12.4].map(stats.formatMs), ['0', '<1', '1.0', '1.5', '10.0', '12'])
  assert.deepEqual([0, 0.123, 4, 12.34].map(stats.formatMbps), ['0', '0.12', '4.00', '12.3'])
  assert.deepEqual([0, 0.05, 1.234].map(stats.formatPct), ['0', '<0.1', '1.2'])
})

test('thresholds escalate at the documented boundaries', () => {
  assert.deepEqual([1, 1.01, 5, 5.01].map(stats.lossLevel), ['ok', 'warning', 'warning', 'bad'])
  assert.deepEqual([100, 101, 250, 251].map(stats.rttLevel), ['ok', 'warning', 'warning', 'bad'])
  assert.equal(stats.fpsLevel(29, 60), 'warning')
  assert.equal(stats.fpsLevel(30, 60), 'ok')
  assert.equal(stats.fpsLevel(1, 5), 'ok', 'a static desktop with a low peak never warns')
})

test('cards snap to the corner nearest their centre', () => {
  assert.equal(stats.nearestCorner(100, 100, 1000, 600), 'top-left')
  assert.equal(stats.nearestCorner(900, 100, 1000, 600), 'top-right')
  assert.equal(stats.nearestCorner(100, 500, 1000, 600), 'bottom-left')
  assert.equal(stats.nearestCorner(900, 500, 1000, 600), 'bottom-right')
})

test('sparklines span the full width and keep zero on the baseline', () => {
  assert.equal(stats.sparklinePoints([5], 72, 16), '')
  const points = stats.sparklinePoints([0, 10, 5], 72, 16).split(' ').map(p => p.split(',').map(Number))
  assert.deepEqual(points.map(([x]) => x), [0, 36, 72])
  assert.equal(points[0][1], 15.5)
  assert.equal(points[1][1], 0.5)
  assert.ok(points.every(([, y]) => y >= 0 && y <= 16))
})
