// Run against the board with PLAYWRIGHT_MODULE, TERMINAL_TEST_URL and MANAGER_PASSWORD.
// An SSH loopback URL may be used when the manager credentials are unavailable.
import assert from 'node:assert/strict'
import fs from 'node:fs'
const { chromium } = await import(process.env.PLAYWRIGHT_MODULE)
const url = process.env.TERMINAL_TEST_URL || 'http://192.168.50.210:8080'
const output = process.env.TERMINAL_TEST_OUTPUT || '/tmp/okvm-ui/term-r1'
fs.mkdirSync(output, { recursive: true })
const browser = await chromium.launch({ channel: 'chrome' })
const rows = [], errors = [], hid = []
const round2 = !!process.env.TERMINAL_TEST_ROUND2
const labels = {
  'zh-CN': { title: '板子终端', more: '更多', maximize: '最大化', restore: '还原', close: '关闭', larger: '放大字号', smaller: '缩小字号', reconnect: '重新连接', fullscreen: '全屏', connected: '已连接', disconnected: '已断开' },
  'en-US': { title: 'Board terminal', more: 'More', maximize: 'Maximize', restore: 'Restore', close: 'Close', larger: 'Larger text', smaller: 'Smaller text', reconnect: 'Reconnect', fullscreen: 'Fullscreen', connected: 'Connected', disconnected: 'Disconnected' },
}
function protect({ layout, language, theme }) {
  if (window === window.top) {
    localStorage.setItem('consoleLayout', layout)
    localStorage.setItem('language', language)
    localStorage.setItem('theme', theme)
  }
  window.blockedHid = []
  window.visibleTerminalOverlays = []
  if (window !== window.top) {
    const inspect = () => {
      if (frameElement?.getBoundingClientRect().width && getComputedStyle(frameElement).visibility !== 'hidden') {
        for (const el of document.querySelectorAll('.xterm > div:not(.xterm-viewport):not(.xterm-screen)')) {
          const css = getComputedStyle(el)
          if (el.getBoundingClientRect().width && css.display !== 'none' && css.visibility !== 'hidden' && Number(css.opacity) > 0) window.visibleTerminalOverlays.push(el.textContent)
        }
      }
      requestAnimationFrame(inspect)
    }
    requestAnimationFrame(inspect)
  }
  RTCDataChannel.prototype.send = function () { window.blockedHid.push('rtc') }
  const send = WebSocket.prototype.send
  const NativeSocket = WebSocket
  window.testSockets = []
  window.WebSocket = class extends NativeSocket {
    constructor(...args) { super(...args); window.testSockets.push(this) }
    send(data) {
      if (new URL(this.url).pathname.includes('/ws/hid')) { window.blockedHid.push('ws'); return }
      return send.call(this, data)
    }
  }
}
async function contextFor(config) {
  const context = await browser.newContext({ viewport: config.viewport, hasTouch: config.touch || false })
  if (!['127.0.0.1', 'localhost', '[::1]'].includes(new URL(url).hostname)) {
    assert(process.env.MANAGER_PASSWORD, 'MANAGER_PASSWORD required')
    const response = await context.request.post(`${url}/__vim4/api/auth/login`, {
      headers: { Origin: new URL(url).origin },
      data: { username: process.env.MANAGER_USERNAME || 'admin', password: process.env.MANAGER_PASSWORD },
    })
    assert(response.ok(), `Manager login: ${response.status()}`)
  }
  await context.route('**/api/hid/**', route => {
    if (route.request().method() === 'GET') return route.continue()
    hid.push({ transport: 'http', url: route.request().url() })
    return route.abort()
  })
  await context.addInitScript(protect, config)
  return context
}
async function open(page, l) {
  await page.waitForFunction(() => document.querySelector('.console-toolbar'))
  await page.waitForTimeout(300)
  if (await page.locator('.console-pill').isVisible()) {
    await page.locator('.console-pill').click()
    await page.waitForTimeout(350)
  }
  const button = page.getByRole('button', { name: l.title, exact: true }).first()
  if (await button.isVisible()) await button.click()
  else {
    await page.getByRole('button', { name: l.more, exact: true }).first().click()
    await page.getByRole('menuitem', { name: l.title, exact: true }).click()
  }
  await page.waitForFunction(() => {
    const f = document.querySelector('[data-board-terminal] iframe')
    return f?.contentWindow.term?.cols > 5 && f.contentWindow.term.buffer.active.getLine(0)?.translateToString().includes('$')
  })
  await page.waitForTimeout(200)
  if (round2) await page.waitForFunction(() => {
    const f = document.querySelector('[data-board-terminal] iframe')
    return f && getComputedStyle(f).visibility === 'visible' && f.contentWindow.term.options.fontSize === Number(localStorage.getItem('terminalFontSize') || 13)
  })
}
async function input(page, text) {
  await page.locator('[data-board-terminal] iframe').evaluate((f, text) => {
    const t = f.contentWindow.term
    if (t.input) t.input(text, true)
    else t._core.coreService.triggerDataEvent(text, true)
  }, text)
  await page.waitForTimeout(150)
}
async function screen(page) {
  return page.locator('[data-board-terminal] iframe').evaluate(f => {
    const t = f.contentWindow.term, b = t.buffer.active
    return Array.from({ length: b.length }, (_, i) => b.getLine(i)?.translateToString(true)).join('\n')
  })
}
async function shot(page, config, state) {
  const { viewport: v, layout, theme, language } = config
  await page.screenshot({ path: `${output}/${v.width}x${v.height}-${layout}-${state}-${theme}${language === 'en-US' ? '-en' : ''}.png` })
}
async function collect(page) {
  for (const frame of page.frames()) {
    hid.push(...await frame.evaluate(() => window.blockedHid || []))
    if (round2) assert.deepEqual(await frame.evaluate(() => window.visibleTerminalOverlays || []), [])
  }
}
try {
  for (const language of ['zh-CN', 'en-US']) for (const theme of ['light', 'dark']) for (const layout of ['current', 'floating', 'sidebar']) for (const viewport of [{ width: 1440, height: 900 }, { width: 1920, height: 1080 }]) {
    const config = { language, theme, layout, viewport }, l = labels[language]
    if (process.env.TERMINAL_TEST_MOBILE_ONLY) continue
    if (process.env.TERMINAL_TEST_SMOKE && !(language === 'zh-CN' && theme === 'light' && layout === 'current' && viewport.width === 1440)) continue
    const context = await contextFor(config), page = await context.newPage()
    page.on('pageerror', e => errors.push(e.message))
    await page.goto(url)
    await page.waitForFunction(() => document.querySelector('video')?.videoWidth > 0)
    await open(page, l)
    await page.waitForTimeout(400)
    const measure = await page.evaluate(() => {
      const panel = document.querySelector('[data-board-terminal]'), workspace = document.querySelector('.video-workspace'), video = document.querySelector('video')
      const rect = el => el.getBoundingClientRect().toJSON(), v = rect(video), p = rect(panel)
      return { panel: p, workspace: rect(workspace), video: v, toolbar: rect(document.querySelector('.console-toolbar')), pointer: !!document.elementFromPoint(v.x + v.width / 2, v.y + v.height / 2)?.closest('[tabindex="0"]'), cols: panel.querySelector('iframe').contentWindow.term.cols, backdrop: document.querySelectorAll('[data-slot="dialog-overlay"]').length }
    })
    assert.equal(measure.panel.width, viewport.width * .45)
    assert(Math.abs(measure.workspace.right - measure.panel.left) <= 1, JSON.stringify(measure))
    assert(Math.abs(measure.video.right - measure.panel.left) <= 1, JSON.stringify(measure))
    assert(measure.pointer); assert.equal(measure.backdrop, 0)
    if (layout === 'floating') assert(measure.toolbar.right <= measure.panel.left)
    await shot(page, config, 'open')
    const cols = measure.cols
    await input(page, 'tput cols\r')
    assert((await screen(page)).includes(`\n${cols}\n`))
    await page.getByRole('button', { name: l.maximize, exact: true }).click()
    await page.waitForTimeout(300)
    assert(await page.locator('[data-board-terminal]').evaluate(el => Math.abs(el.clientWidth - el.parentElement.clientWidth) <= 1))
    assert(await page.locator('video').evaluate(el => el.getBoundingClientRect().width === 0))
    await shot(page, config, 'maximised')
    await page.getByRole('button', { name: l.restore, exact: true }).click()
    // All layouts also exercise the actual fullscreen capture path.
    await page.evaluate(() => document.documentElement.requestFullscreen())
    await page.waitForTimeout(300)
    assert(await page.locator('[data-board-terminal]').evaluate(el => el.getBoundingClientRect().top === 0))
    await page.frameLocator('iframe').locator('textarea').press('Escape')
    await page.frameLocator('iframe').locator('textarea').press('Control+c')
    assert(await page.locator('[data-board-terminal]').isVisible())
    if (!await page.evaluate(() => !!document.fullscreenElement)) await page.evaluate(() => document.documentElement.requestFullscreen())
    await shot(page, { ...config, layout: `${layout}-fullscreen` }, 'open')
    await page.getByRole('button', { name: l.maximize, exact: true }).click()
    await page.waitForTimeout(150)
    await shot(page, { ...config, layout: `${layout}-fullscreen` }, 'maximised')
    await page.getByRole('button', { name: l.restore, exact: true }).click()
    await page.evaluate(() => document.exitFullscreen())
    // Close and reopen without unloading or changing shell environment.
    await input(page, 'export MARK=$RANDOM; echo MARK=$MARK\r')
    const marker = (await screen(page)).match(/\nMARK=(\d+)/)?.[1]
    assert(marker)
    await page.getByRole('button', { name: l.close, exact: true }).click()
    await open(page, l)
    await input(page, 'echo AGAIN=$MARK\r')
    assert((await screen(page)).includes(`AGAIN=${marker}`))
    const before = await page.locator('iframe').evaluate(f => f.contentWindow.term.options.theme.background)
    await page.evaluate(() => document.documentElement.classList.toggle('dark'))
    await page.waitForTimeout(200)
    assert.notEqual(await page.locator('iframe').evaluate(f => f.contentWindow.term.options.theme.background), before)
    await page.evaluate(() => document.documentElement.classList.toggle('dark'))
    await page.getByRole('button', { name: l.larger, exact: true }).click()
    assert.equal(await page.locator('iframe').evaluate(f => f.contentWindow.term.options.fontSize), 14)
    if (round2) {
      await page.waitForTimeout(400)
      await shot(page, config, 'after-font-change')
      assert.equal(await page.locator('[data-board-terminal] > header').evaluate(e => e.getBoundingClientRect().height), 36)
    }
    assert.equal(await page.evaluate(() => localStorage.getItem('terminalFontSize')), '14')
    await page.getByRole('button', { name: l.smaller, exact: true }).click()
    // Widening and persistence: start narrow enough for 700 to be an increase at both widths.
    const handle = page.locator('.terminal-resizer')
    let narrowCols
    for (const target of [500, 700]) {
      const box = await handle.boundingBox()
      await page.mouse.move(box.x + 3, box.y + 100)
      await page.mouse.down(); await page.mouse.move(viewport.width - target, box.y + 100); await page.mouse.up()
      await page.waitForTimeout(200)
      if (target === 500) {
        await input(page, 'tput cols\r')
        narrowCols = await page.locator('iframe').evaluate(f => f.contentWindow.term.cols)
      }
    }
    const wideCols = await page.locator('iframe').evaluate(f => f.contentWindow.term.cols)
    assert(wideCols > narrowCols)
    await input(page, 'tput cols\r')
    assert((await screen(page)).includes(`\n${wideCols}\n`))
    assert.equal(await page.evaluate(() => localStorage.getItem('terminalPanelWidth')), '700')
    await collect(page)
    await page.getByRole('button', { name: l.larger, exact: true }).click()
    await page.reload()
    await page.waitForFunction(() => document.querySelector('video')?.videoWidth > 0)
    await open(page, l)
    assert.equal(await page.locator('[data-board-terminal]').evaluate(el => el.getBoundingClientRect().width), 700)
    assert.equal(await page.locator('iframe').evaluate(f => f.contentWindow.term.options.fontSize), 14)
    await page.getByRole('button', { name: l.smaller, exact: true }).click()
    // The proxy converts close(1000) into 1006 and ttyd reconnects immediately.
    // Close the real transport and simulate its normal close event to hold the
    // disconnected state for inspection (ttyd removes its listeners on close).
    await page.locator('iframe').evaluate(f => {
      const s = f.contentWindow.testSockets.find(s => s.readyState === 1 && s.url.includes('/api/terminal/ws'))
      s.close(1000)
      s.dispatchEvent(new f.contentWindow.CloseEvent('close', { code: 1000 }))
    })
    await page.waitForFunction(text => document.querySelector('[data-terminal-status]')?.textContent === text, l.disconnected)
    await page.getByRole('button', { name: l.larger, exact: true }).click()
    await page.waitForTimeout(600)
    assert.equal(await page.locator('[data-terminal-status]').textContent(), l.disconnected)
    await page.getByRole('button', { name: l.smaller, exact: true }).click()
    await shot(page, config, 'disconnected')
    let releaseReconnect
    let reconnectRoute
    if (round2) {
      assert.equal(await page.locator('.terminal-disconnected').evaluate(e => e.getBoundingClientRect().height), 32)
      assert.equal(await page.locator('iframe').evaluate(e => getComputedStyle(e).opacity), '0.5')
      assert.equal(await page.locator('iframe').evaluate(e => e.contentWindow.term.options.cursorBlink), false)
      await page.frameLocator('iframe').locator('textarea').press('Enter')
      await page.waitForTimeout(200)
      assert.equal(await page.locator('[data-terminal-status]').textContent(), l.disconnected)
      assert.equal(await page.locator('iframe').evaluate(f => f.contentWindow.testSockets.filter(s => s.readyState === 1 && s.url.includes('/api/terminal/ws')).length), 0)
      const gate = new Promise(resolve => { releaseReconnect = resolve })
      reconnectRoute = async route => { await gate; await route.continue() }
      await page.route('**/api/terminal/', reconnectRoute)
    }
    await page.getByRole('button', { name: l.reconnect, exact: true }).click()
    if (round2) {
      assert(await page.locator('.terminal-disconnected button').isDisabled())
      assert(await page.locator('.terminal-disconnected .animate-spin').isVisible())
      await shot(page, config, 'reconnecting')
      releaseReconnect()
    }
    await page.waitForFunction(text => document.querySelector('[data-terminal-status]')?.textContent === text, l.connected)
    if (round2) await page.unroute('**/api/terminal/', reconnectRoute)
    await page.locator('.terminal-resizer').dblclick()
    assert.equal(await page.evaluate(() => localStorage.getItem('terminalPanelWidth')), null)
    assert.equal(await page.locator('[data-board-terminal]').evaluate(el => el.getBoundingClientRect().width), viewport.width * .45)
    rows.push({ ...config, ...measure, narrowCols, wideCols, marker, persistentWidth: 700 })
    await collect(page); await context.close()
    console.log('PASS desktop', viewport.width, layout, language, theme)
  }
  for (const language of ['zh-CN', 'en-US']) for (const theme of ['light', 'dark']) for (const viewport of [{ width: 390, height: 844 }, { width: 844, height: 390 }]) {
    const config = { language, theme, layout: 'current', viewport, touch: true }, l = labels[language]
    if (process.env.TERMINAL_TEST_SMOKE && !(language === 'zh-CN' && theme === 'light' && viewport.width === 390)) continue
    const context = await contextFor(config), page = await context.newPage()
    page.on('pageerror', e => errors.push(e.message))
    await page.goto(url); await open(page, l)
    const rect = await page.locator('[data-board-terminal]').boundingBox()
    assert.equal(rect.x, 0); assert.equal(rect.y, 0); assert.equal(rect.width, viewport.width); assert.equal(rect.height, viewport.height)
    assert(await page.locator('.terminal-keys').isVisible())
    assert((await screen(page)).includes('khadas@Khadas'))
    await input(page, 'echo PREVIOUS\r')
    await page.locator('.terminal-keys').getByRole('button', { name: '↑', exact: true }).tap()
    await input(page, '\r')
    assert((await screen(page)).split('\nPREVIOUS\n').length >= 3)
    await input(page, 'ech')
    await page.locator('.terminal-keys').getByRole('button', { name: 'Tab', exact: true }).tap()
    await input(page, ' COMPLETED\r')
    assert((await screen(page)).includes('\nCOMPLETED\n'))
    await page.locator('.terminal-keys').getByRole('button', { name: 'Ctrl', exact: true }).tap()
    await page.locator('iframe').evaluate(f => { f.contentWindow.term.testInput = []; f.contentWindow.term.onData(data => f.contentWindow.term.testInput.push(data)) })
    await page.frameLocator('iframe').locator('textarea').press('c')
    assert.deepEqual(await page.locator('iframe').evaluate(f => f.contentWindow.term.testInput), ['\x03'])
    // Exercise keyboard viewport changes independently of Chrome's desktop OS.
    await page.evaluate(() => {
      Object.defineProperty(visualViewport, 'height', { configurable: true, value: 240 })
      Object.defineProperty(visualViewport, 'offsetTop', { configurable: true, value: 20 })
      visualViewport.dispatchEvent(new Event('resize'))
    })
    await page.waitForTimeout(100)
    assert.equal((await page.locator('[data-board-terminal]').boundingBox()).height, 240)
    assert.equal((await page.locator('[data-board-terminal]').boundingBox()).y, 20)
    const keyBox = await page.locator('.terminal-keys').boundingBox()
    assert(keyBox.y + keyBox.height <= 260)
    await page.evaluate(() => {
      delete visualViewport.height; delete visualViewport.offsetTop
      visualViewport.dispatchEvent(new Event('resize'))
    })
    await page.waitForTimeout(100)
    assert.equal(await page.locator('.terminal-keys').getByRole('button', { name: 'Ctrl', exact: true }).getAttribute('aria-pressed'), 'false')
    await page.locator('.terminal-keys').getByRole('button', { name: 'Ctrl', exact: true }).tap()
    await page.locator('iframe').evaluate(f => {
      f.contentWindow.term.testInput = []
      f.contentDocument.activeElement.dispatchEvent(new f.contentWindow.InputEvent('beforeinput', { data: 'c', inputType: 'insertText', bubbles: true, cancelable: true }))
    })
    assert.deepEqual(await page.locator('iframe').evaluate(f => f.contentWindow.term.testInput), ['\x03'])
    if (round2) {
      const row = page.locator('.terminal-keys')
      const overflow = await row.evaluate(e => e.scrollWidth > e.clientWidth)
      if (overflow) {
        await row.evaluate(e => e.scrollLeft = 0)
        await page.waitForTimeout(150)
        assert(!await page.locator('.terminal-key-fade--left').isVisible())
        assert(await page.locator('.terminal-key-fade--right').isVisible())
        await row.evaluate(e => e.scrollLeft = (e.scrollWidth - e.clientWidth) / 2)
        await page.waitForTimeout(150)
        assert(await page.locator('.terminal-key-fade--left').isVisible())
        assert(await page.locator('.terminal-key-fade--right').isVisible())
        await shot(page, config, 'mobile-keys-scrolled-middle')
        await row.evaluate(e => e.scrollLeft = e.scrollWidth)
        await page.waitForTimeout(150)
        assert(!await page.locator('.terminal-key-fade--right').isVisible())
      } else {
        assert(!await page.locator('.terminal-key-fade--left').isVisible())
        assert(!await page.locator('.terminal-key-fade--right').isVisible())
        await shot(page, config, 'mobile-keys')
      }
    } else await shot(page, config, 'mobile-keys')
    await collect(page); rows.push({ ...config, rect }); await context.close()
    console.log('PASS mobile', viewport.width, language, theme)
  }
  assert.deepEqual(errors, []); assert.deepEqual(hid, [])
} finally {
  fs.writeFileSync(`${output}/measurements.json`, JSON.stringify({ rows, errors, hid, url }, null, 2))
  await browser.close()
}
