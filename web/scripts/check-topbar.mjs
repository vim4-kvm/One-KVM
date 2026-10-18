// Offline browser regression check; run against a local Vite server only.
// PLAYWRIGHT_MODULE=/path/to/playwright/index.mjs node web/scripts/check-topbar.mjs
import assert from 'node:assert/strict'
import { mkdir, writeFile } from 'node:fs/promises'
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE || 'playwright')
const output = new URL('../../../logs/unified-auth/shots-onekvm-topbar/', import.meta.url)
await mkdir(output, { recursive: true })
const browser = await chromium.launch({ channel: 'chrome', headless: true })
const boxes = {}
const activity = (state, client = null) => ({ state, client, tool: state === 'active' ? 'type' : null, detail: state === 'active' ? '18 chars' : null, since: new Date().toISOString() })
const platform = { mode_label: 'VIM4 Ubuntu', msd: { available: true }, hid: { available: true }, audio: { available: true }, video: { available: true }, atx: { available: false }, extensions: { available: true } }

async function openConsole({ managed = true, phone = false, dark = false } = {}) {
  const context = await browser.newContext({ viewport: phone ? { width: 390, height: 844 } : { width: 1440, height: 900 }, hasTouch: phone, isMobile: phone })
  await context.addInitScript(({ dark }) => {
    localStorage.setItem('language', 'zh-CN')
    localStorage.setItem('theme', dark ? 'dark' : 'light')
  }, { dark })
  const page = await context.newPage()
  page.setDefaultTimeout(10000)
  const errors = []
  page.on('pageerror', error => { errors.push(error.message); console.error(error.message) })
  let socket
  let requests = 0
  let pendingSnapshot
  let holdSnapshot = false
  let failSnapshot = false
  let snapshot = { enabled: true, credentials_error: null, activity: activity('idle') }
  await page.routeWebSocket('**/api/**', ws => {
    if (new URL(ws.url()).pathname === '/api/ws') socket = ws
  })
  await page.route('http://127.0.0.1:5178/api/**', async route => {
    const path = new URL(route.request().url()).pathname
    if (process.env.DEBUG_TOPBAR) console.log(path)
    let data = {}
    if (path === '/api/mcp/status') {
      requests++
      if (failSnapshot) return route.fulfill({ status: 503, json: { message: 'offline test' } })
      const captured = structuredClone(snapshot)
      if (holdSnapshot) await new Promise(resolve => { pendingSnapshot = resolve })
      data = captured
    } else if (path === '/api/setup') data = { initialized: true, needs_setup: false, platform }
    else if (path === '/api/auth/check') data = { authenticated: true, user: managed ? 'vim4-managed' : 'admin', auth_mode: managed ? 'managed' : 'native' }
    else if (path === '/api/info') data = { version: 'test', initialized: true, platform, capabilities: platform }
    else if (path === '/api/msd/status') data = { available: true, state: { disk_mode: 'image', mounted_count: 0, mounted_media: [], slot_capacity: 1 } }
    else if (path === '/api/hid/status') data = { available: true, initialized: true, online: true, backend: 'otg', supports_absolute_mouse: true, led_state: {} }
    else if (path === '/api/stream/status') data = { state: 'streaming', resolution: [1920, 1080], target_fps: 60, stream_mode: 'mjpeg' }
    else if (path === '/api/stream/mode') data = { mode: 'mjpeg' }
    else if (path === '/api/config/hid') data = { backend: 'otg', mouse: { absolute: true } }
    else if (path === '/api/stream/constraints') data = { constraints: [] }
    else if (path.includes('/mjpeg')) return route.abort()
    await route.fulfill({ json: data })
  })
  await page.goto('http://127.0.0.1:5178/')
  const card = page.getByRole('button', { name: /^MCP:/ })
  if (!phone) await card.waitFor().catch(async error => { console.log(await page.locator('body').innerText()); throw error })
  else await page.getByRole('button', { name: '更多', exact: true }).waitFor()
  return { context, page, card, errors,
    emit: next => socket.send(JSON.stringify({ event: 'mcp.activity', data: next })),
    async reconnect(next, hold = false, fail = false) {
      snapshot = next
      holdSnapshot = hold
      failSnapshot = fail
      const before = requests
      socket.close()
      await page.waitForFunction(() => document.body.textContent.includes('连接中'))
      const deadline = Date.now() + 10000
      while (requests === before) {
        assert.ok(Date.now() < deadline, 'WebSocket must reconnect and request a snapshot')
        await new Promise(resolve => setTimeout(resolve, 20))
      }
    },
    release: () => { holdSnapshot = false; pendingSnapshot() },
  }
}
async function shot(page, name) {
  await page.screenshot({ path: new URL(`${name}.png`, output).pathname, fullPage: true, animations: 'disabled' })
}
async function label(card, text) {
  await card.filter({ hasText: text }).waitFor()
}
try {
  const test = await openConsole()
  const { page, card } = test
  assert.equal(await page.getByRole('button', { name: /vim4-managed/ }).count(), 0)
  const theme = page.getByRole('button', { name: '切换主题', exact: true })
  const longClient = 'Research assistant with a very long client name for the fixed width regression'
  for (const [state, text] of [['idle', '待命'], ['active', '正在操作'], ['manual', '你已接管']]) {
    test.emit(activity(state, state === 'active' ? longClient : null))
    await label(card, text)
    boxes[state] = await theme.boundingBox()
    assert.equal((await card.boundingBox()).width, 100)
    const msdBox = await page.getByRole('button', { name: /^MSD:/ }).boundingBox()
    assert.equal((await card.boundingBox()).width, msdBox.width)
    assert.equal((await card.boundingBox()).height, msdBox.height)
    const dot = card.locator('span.rounded-full')
    assert.equal(await dot.evaluate(el => el.classList.contains('animate-pulse')), state === 'active')
    assert.equal(await dot.evaluate(el => el.classList.contains('bg-muted-foreground')), state !== 'active')
    await shot(page, `desktop-mcp-${state}`)
  }
  assert.equal(boxes.idle.x, boxes.active.x)
  assert.equal(boxes.idle.x, boxes.manual.x)
  test.emit(activity('active', longClient))
  await label(card, '正在操作')
  await card.hover()
  await page.getByRole('link', { name: 'MCP 设置' }).waitFor()
  assert.equal(await page.getByRole('link', { name: 'MCP 设置' }).getAttribute('href'), '/settings?tab=ext-mcp')
  await shot(page, 'desktop-mcp-hover')
  await page.mouse.move(10, 400)
  // The response captured before this event must not overwrite the newer activity.
  await test.reconnect({ enabled: true, credentials_error: null, activity: activity('idle') }, true)
  await label(card, '连接中')
  test.emit(activity('active', 'Claude'))
  test.release()
  await label(card, 'Claude 正在操作')
  await test.reconnect({ enabled: true, credentials_error: null, activity: activity('idle') }, false, true)
  await label(card, '连接中')
  assert.equal((await theme.boundingBox()).x, boxes.idle.x)
  await test.reconnect({ enabled: true, credentials_error: 'unreadable', activity: activity('idle') })
  await label(card, '密钥读取失败')
  assert.equal(await card.locator('span.rounded-full').evaluate(el => el.classList.contains('bg-destructive')), true)
  await theme.click()
  await shot(page, 'desktop-mcp-error')
  await test.reconnect({ enabled: false, credentials_error: null, activity: activity('idle') })
  await card.waitFor({ state: 'hidden' })
  await shot(page, 'desktop-mcp-off')
  assert.deepEqual(test.errors, [])
  await test.context.close()

  const phone = await openConsole({ phone: true })
  await phone.page.getByRole('button', { name: '更多', exact: true }).click()
  await phone.page.getByRole('menuitem').first().waitFor()
  assert.equal(await phone.page.getByRole('menuitem').count(), 2)
  assert.equal(await phone.page.getByText('修改密码', { exact: true }).count(), 0)
  await shot(phone.page, 'phone-managed-menu')
  await phone.page.keyboard.press('Escape')
  await phone.page.getByRole('button', { name: /^连接详情/ }).click()
  await phone.page.getByRole('link', { name: 'MCP 设置' }).waitFor()
  await shot(phone.page, 'phone-mcp-details')
  await phone.page.keyboard.press('Escape')
  await phone.page.getByRole('button', { name: '更多', exact: true }).click()
  await phone.page.getByRole('menuitem').first().click()
  assert.equal(await phone.page.locator('html').evaluate(el => el.classList.contains('dark')), true)
  await phone.page.getByRole('button', { name: '更多', exact: true }).click()
  await phone.page.getByRole('menuitem').last().click()
  await phone.page.getByRole('button', { name: 'More', exact: true }).waitFor()
  assert.deepEqual(phone.errors, [])
  await phone.context.close()

  const native = await openConsole({ managed: false })
  await native.page.getByRole('button', { name: 'admin', exact: true }).click()
  await native.page.getByRole('menuitem', { name: '修改密码' }).waitFor()
  await shot(native.page, 'desktop-native')
  await native.page.getByRole('menuitem', { name: '修改密码' }).click()
  await native.page.getByRole('dialog').waitFor()
  await native.page.keyboard.press('Escape')
  await native.page.setViewportSize({ width: 390, height: 844 })
  await native.page.getByRole('button', { name: 'admin', exact: true }).click()
  await native.page.getByRole('menuitem', { name: '深色模式' }).waitFor()
  assert.deepEqual(native.errors, [])
  await native.context.close()
  await writeFile(new URL('bounding-boxes.json', output), JSON.stringify(boxes, null, 2))
  console.log(JSON.stringify({ passed: true, themeButtonBoundingBoxes: boxes }, null, 2))
} finally {
  await browser.close()
}
