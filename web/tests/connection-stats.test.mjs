import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import ts from 'typescript'
import { test } from 'node:test'

const source = readFileSync(new URL('../src/lib/connectionStats.ts', import.meta.url), 'utf8')
const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext } }).outputText
const stats = await import(`data:text/javascript;base64,${Buffer.from(compiled).toString('base64')}`)

const raw = (over = {}) => ({ bytesReceived: 0, packetsReceived: 0, packetsLost: 0, framesPerSecond: 60, jitter: 0.002, roundTripTime: 0.012, ...over })

test('sampler derives per-interval bitrate and loss from cumulative counters', () => {
  const sample = stats.createConnectionSampler()
  const first = sample(raw({ bytesReceived: 1_000_000, packetsReceived: 1000 }), 1000)
  assert.equal(first.bitrateMbps, 0)
  assert.equal(first.lossPct, 0)
  const second = sample(raw({ bytesReceived: 2_000_000, packetsReceived: 1990, packetsLost: 10 }), 2000)
  assert.equal(second.bitrateMbps, 8)
  assert.equal(second.lossPct, 1)
  assert.equal(second.rttMs, 12)
  assert.equal(second.jitterMs, 2)
})

test('a counter reset from a new peer connection starts a fresh baseline instead of negative rates', () => {
  const sample = stats.createConnectionSampler()
  sample(raw({ bytesReceived: 5_000_000, packetsReceived: 5000, packetsLost: 50 }), 1000)
  const reset = sample(raw({ bytesReceived: 10_000, packetsReceived: 10 }), 2000)
  assert.equal(reset.bitrateMbps, 0)
  assert.equal(reset.lossPct, 0)
  const next = sample(raw({ bytesReceived: 135_000, packetsReceived: 110 }), 3000)
  assert.equal(next.bitrateMbps, 1)
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
