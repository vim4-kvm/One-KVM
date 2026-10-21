import assert from 'node:assert/strict'
import { readFileSync } from 'node:fs'
import vm from 'node:vm'
import ts from 'typescript'
import { test } from 'node:test'

// Run the production handlers with controlled transport calls and event order.
// Vue mounting is unnecessary: the regression concerns closing a healthy peer.
const vue = readFileSync(new URL('../src/views/ConsoleView.vue', import.meta.url), 'utf8')
const script = vue.slice(vue.indexOf('>') + 1, vue.indexOf('</script>'))
const ast = ts.createSourceFile('console.ts', script, ts.ScriptTarget.Latest, true)
const names = ['handleStreamRecovered', 'handleStreamConfigApplied',
  'handleStreamModeSwitching', 'handleStreamModeReady', 'handleStreamConfigChanging']
const handlers = ast.statements.filter(s => ts.isFunctionDeclaration(s) && names.includes(s.name?.text))
assert.equal(handlers.length, names.length)
const code = ts.transpileModule(handlers.map(s => s.getText(ast)).join('\n'), {
  compilerOptions: { target: ts.ScriptTarget.ES2022, module: ts.ModuleKind.None },
}).outputText

function fixture() {
  const calls = { switch: 0, rebind: 0, connect: 0, refresh: 0 }
  const context = vm.createContext({
    videoMode: { value: 'h265' }, webrtc: {
      isConnected: { value: true }, isConnecting: { value: false },
      state: { value: 'connected' }, connectStage: { value: 'connected' },
      sessionId: { value: 'existing-peer' },
    },
    videoRestarting: { value: false }, videoLoading: { value: false },
    videoError: { value: false }, videoErrorMessage: { value: '' },
    isModeSwitching: { value: false }, consecutiveErrors: 0,
    retryTimeoutId: null, retryCount: 0, backendFps: { value: 0 },
    gracePeriodTimeoutId: null, GRACE_PERIOD: 100,
    videoDebugLog() {}, cancelWebRTCRecovery() {}, console: { log() {} },
    window: { setTimeout() { return 1 } },
    pendingWebRTCReadyGate: false,
    async captureFrameOverlay() {},
    async rebindWebRTCVideo() { calls.rebind++ },
    async connectWebRTCSerial() { calls.connect++; return true },
    async switchToWebRTC() { calls.switch++ },
    refreshVideo() { calls.refresh++ },
  })
  context.videoSession = {
    backendSwitching: { value: false }, localSwitching: context.isModeSwitching,
    onModeSwitching() { this.backendSwitching.value = true },
    onModeReady() { this.backendSwitching.value = false },
  }
  vm.runInContext('let recoveryEventHandled = false;\n' + code, context)
  return { context, calls }
}

test('duplicate source recovery bookkeeping preserves the existing peer', async () => {
  const { context, calls } = fixture()
  context.handleStreamRecovered({ device: '/dev/video_cap' })
  const input = { device: '/dev/video_cap', resolution: [3840, 2160], fps: 60 }
  await context.handleStreamConfigApplied(input)
  await context.handleStreamConfigApplied(input)
  assert.equal(calls.rebind, 1)
  assert.equal(calls.switch, 0)
  assert.equal(calls.connect, 0)
})

test('input discovery without a requested change does not renegotiate a healthy peer', async () => {
  const { context, calls } = fixture()
  await context.handleStreamConfigApplied({ device: '/dev/video_cap' })
  assert.equal(calls.switch, 0)
})

test('input discovery before backend mode_ready cannot start a second transition', async () => {
  const { context, calls } = fixture()
  const transition = { transition_id: 'initial-1', from_mode: 'h264', to_mode: 'h265' }
  context.handleStreamModeSwitching(transition)
  const input = { device: '/dev/video_cap', resolution: [3840, 2160], fps: 60 }
  await context.handleStreamConfigApplied(input)
  await context.handleStreamConfigApplied(input)
  assert.equal(context.videoSession.backendSwitching.value, true)
  assert.equal(calls.switch, 0)
  context.handleStreamModeReady({ transition_id: 'initial-1', mode: 'h265' })
  assert.equal(context.videoLoading.value, false)
  await context.handleStreamConfigApplied(input)
  assert.equal(context.videoRestarting.value, false)
  assert.equal(calls.switch, 0)
})

test('recovery during a backend transition does not suppress a later requested change', async () => {
  const { context, calls } = fixture()
  context.handleStreamModeSwitching({ transition_id: 'initial-1', from_mode: 'h264', to_mode: 'h265' })
  context.handleStreamRecovered({ device: '/dev/video_cap' })
  await context.handleStreamConfigApplied({ device: '/dev/video_cap' })
  context.handleStreamModeReady({ transition_id: 'initial-1', mode: 'h265' })
  context.handleStreamConfigChanging({ reason: 'user configuration' })
  await context.handleStreamConfigApplied({ device: '/dev/video_cap' })
  assert.equal(calls.switch, 1)
})

test('mode_ready keeps loading while a peer or a different codec is still pending', () => {
  for (const mode of ['h264', 'h265', 'mjpeg']) {
    const { context } = fixture()
    context.videoLoading.value = true
    if (mode === 'h265') context.webrtc.isConnected.value = false
    context.handleStreamModeReady({ transition_id: 'pending-1', mode })
    assert.equal(context.videoLoading.value, true)
  }
})

test('explicit configuration changes and disconnected clients still reconnect', async () => {
  for (const change of ['pending', 'transition', 'disconnected', 'disconnected-transition']) {
    const { context, calls } = fixture()
    if (change === 'pending') context.videoRestarting.value = true
    if (change.startsWith('disconnected')) context.webrtc.isConnected.value = false
    if (change === 'disconnected-transition') context.videoSession.backendSwitching.value = true
    await context.handleStreamConfigApplied(change === 'transition' ? { transition_id: 'change-1' } : {})
    assert.equal(calls.switch, 1, change)
  }
})

test('MJPEG and local codec transitions retain their own recovery flow', async () => {
  const { context, calls } = fixture()
  context.videoMode.value = 'mjpeg'
  context.webrtc.isConnected.value = false
  context.handleStreamRecovered({})
  await context.handleStreamConfigApplied({})
  assert.equal(calls.refresh, 1)
  context.videoMode.value = 'h265'
  context.isModeSwitching.value = true
  await context.handleStreamConfigApplied({})
  assert.equal(calls.switch, 0)
})
