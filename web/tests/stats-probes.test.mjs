import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import ts from 'typescript'
import { test } from 'node:test'

const load = async name => {
  const source = readFileSync(new URL(`../src/lib/${name}.ts`, import.meta.url), 'utf8')
  const compiled = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext, target: ts.ScriptTarget.ESNext } }).outputText
  return import(`data:text/javascript;base64,${Buffer.from(compiled).toString('base64')}`)
}
const cadence = await load('frameCadence')
const ping = await load('dataChannelPing')

const feed = (c, frames) => frames.forEach(([presentationTime, presentedFrames, receiveTime]) => c.add({ presentationTime, presentedFrames, receiveTime }))
const close = (a, b) => assert.ok(Math.abs(a - b) < 1e-9, `${a} != ${b}`)

test('presented frame rate and gaps cover the trailing second', () => {
  const c = cadence.createFrameCadence()
  feed(c, Array.from({ length: 121 }, (_, i) => [1000 + i * 1000 / 60, i + 1, 1000 + i * 1000 / 60 - 3]))
  const s = c.summarize(3000)
  close(s.presentedFps, 60)
  close(s.maxFrameGapMs, 1000 / 60)
  assert.equal(s.stutters, 0)
  close(s.receiveToPresentMs, 3)
})

test('a stall inside the window shows as the longest gap and a stutter', () => {
  const c = cadence.createFrameCadence()
  feed(c, [[0, 1], [16, 2], [32, 3], [152, 4], [168, 5]])
  const s = c.summarize(170)
  assert.equal(s.maxFrameGapMs, 120)
  assert.equal(s.stutters, 1)
  assert.equal(s.receiveToPresentMs, 0, 'no receiveTime means no delay figure')
})

test('skipped callbacks are spread over the frames that still presented', () => {
  const c = cadence.createFrameCadence()
  feed(c, [[0, 1], [16, 2], [96, 7], [112, 8]])
  const s = c.summarize(115)
  assert.equal(s.maxFrameGapMs, 16)
  assert.equal(s.stutters, 0)
})

test('no frames for a whole window reports the ongoing stall instead of stale numbers', () => {
  const c = cadence.createFrameCadence()
  feed(c, [[0, 1], [16, 2]])
  assert.deepEqual(c.summarize(1500), { presentedFps: 0, maxFrameGapMs: 1484, stutters: 1, receiveToPresentMs: 0 })
  const fresh = cadence.createFrameCadence()
  assert.equal(fresh.summarize(10), null)
  feed(fresh, [[0, 1]])
  assert.equal(fresh.summarize(10), null, 'one frame is not a rate yet')
})

test('a new stream restarts the frame counter without negative rates', () => {
  const c = cadence.createFrameCadence()
  feed(c, [[0, 500], [16, 501], [32, 2], [48, 3]])
  close(c.summarize(50).presentedFps, 62.5)
})

test('pings round-trip one at a time and average inside the window', () => {
  const p = ping.createPingTracker()
  const first = p.next(100)
  assert.equal(first.byteLength, 13)
  assert.equal(new Uint8Array(first)[0], ping.MSG_PING)
  assert.equal(p.next(200), null, 'one ping in flight')
  const pong = first.slice(0)
  new Uint8Array(pong)[0] = ping.MSG_PONG
  assert.equal(p.receive(pong, 101.5), true)
  assert.equal(p.receive(pong, 102), true, 'a duplicate pong is consumed but not counted twice')
  const second = p.next(600)
  const pong2 = second.slice(0)
  new Uint8Array(pong2)[0] = ping.MSG_PONG
  p.receive(pong2, 603.5)
  close(p.average(700), 2.5)
  close(p.average(1200), 3.5)
  assert.equal(p.average(1700), null)
})

test('a lost ping times out and late or foreign replies are ignored', () => {
  const p = ping.createPingTracker()
  const lost = p.next(0)
  assert.equal(p.next(1999), null)
  const retry = p.next(2000)
  assert.ok(retry)
  const late = lost.slice(0)
  new Uint8Array(late)[0] = ping.MSG_PONG
  p.receive(late, 2001)
  assert.equal(p.average(2001), null)
  assert.equal(p.receive(new Uint8Array([0x00]).buffer, 1), false)
  assert.equal(p.receive(retry, 2002), false, 'our own ping type is not a pong')
})
