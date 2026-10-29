import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import ts from 'typescript'
import { test } from 'node:test'

const source = readFileSync(new URL('../src/lib/ttydClient.ts', import.meta.url), 'utf8')
const code = ts.transpileModule(source, { compilerOptions: { module: ts.ModuleKind.ESNext } }).outputText
const { configureTtydClient } = await import(`data:text/javascript;base64,${Buffer.from(code).toString('base64')}`)

test('native overlays and Enter reconnect are disabled without changing connected input', () => {
  const socket = new EventTarget(), terminal = {}, calls = []
  const originalClose = () => calls.push('native Enter reconnect')
  socket.addEventListener('close', originalClose)
  const client = {
    socket, terminal, resizeOverlay: true, reconnect: true, doReconnect: true,
    onSocketClose: originalClose, dispose: () => calls.push('dispose'),
    overlayAddon: { overlayNode: { style: {}, remove: () => calls.push('remove overlay') }, showOverlay: () => calls.push('overlay') },
  }
  const doc = { body: { __k: { __k: [null, { __c: { xterm: client } }] } } }
  Object.defineProperty(client.overlayAddon, 'showOverlay', { configurable: true, get: () => () => calls.push('overlay') })
  const bound = configureTtydClient(doc, terminal, () => calls.push('disconnected'), () => calls.push('preferences'))
  socket.dispatchEvent(new MessageEvent('message', { data: new Uint8Array([48]).buffer }))
  socket.dispatchEvent(new MessageEvent('message', { data: new Uint8Array([50]).buffer }))
  assert.equal(calls.pop(), 'preferences')
  client.overlayAddon.showOverlay('121x58')
  assert.deepEqual(calls, ['remove overlay'])
  assert.equal(client.resizeOverlay, false)
  assert.equal(client.reconnect, false)
  assert.equal(client.doReconnect, false)
  socket.dispatchEvent(new Event('close'))
  assert.deepEqual(calls, ['remove overlay', 'dispose', 'disconnected'])
  bound.cleanup()
  socket.dispatchEvent(new Event('close'))
  assert.equal(calls.length, 3)
})
test('does not configure another terminal instance', () => {
  assert.equal(configureTtydClient({ body: { __k: { __c: { xterm: { terminal: {} } } } } }, {}, () => {}), undefined)
})
