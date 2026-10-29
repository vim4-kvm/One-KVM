<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { AlertCircle, ExternalLink, Maximize2, Minimize2, RotateCw, Terminal, X } from 'lucide-vue-next'
import { Button } from '@/components/ui/button'
import { consumeTerminalKey, toggleTerminalModifier } from '@/lib/terminalKeys'
import { configureTtydClient } from '@/lib/ttydClient'

const props = defineProps<{ open: boolean }>()
const emit = defineEmits<{
  (e: 'update:open', value: boolean): void
  (e: 'maximized', value: boolean): void
  (e: 'width', value: number): void
}>()
const { t } = useI18n()
type Term = {
  options: { fontSize: number; fontFamily: string; lineHeight: number; cursorBlink: boolean; disableStdin: boolean; theme: Record<string, string> }
  focus(): void
  fit(): void
  input?(data: string, wasUserInput?: boolean): void
  _core: { coreService: { isCursorHidden: boolean; triggerDataEvent(data: string, wasUserInput?: boolean): void } }
  rows: number
  refresh(start: number, end: number): void
  attachCustomKeyEventHandler(handler: (event: KeyboardEvent) => boolean): void
}
const panel = ref<HTMLElement>()
const frame = ref<HTMLIFrameElement>()
const mounted = ref(false)
const maximized = ref(false)
function isMobile() { return window.innerWidth < 768 || window.matchMedia('(pointer: coarse) and (max-height: 500px)').matches }
const mobile = ref(isMobile())
const viewportWidth = ref(window.innerWidth)
const viewportTop = ref(0)
const viewportHeight = ref(window.innerHeight)
const width = ref(Number(localStorage.getItem('terminalPanelWidth')) || 0)
const fontSize = ref(Math.min(20, Math.max(11, Number(localStorage.getItem('terminalFontSize')) || 13)))
const status = ref<'connected' | 'connecting' | 'disconnected'>('connecting')
const host = ref('')
const terminalReady = ref(false)
const showConnectionBar = ref(false)
const keyRow = ref<HTMLElement>()
const fadeLeft = ref(false)
const fadeRight = ref(false)
const modifiers = ref({ ctrl: false, alt: false })
const keys = ['Esc', 'Tab', 'Ctrl', 'Alt', '↑', '↓', '←', '→', '|', '~', '/', '-']
const panelWidth = computed(() => Math.min(Math.max(480, width.value || viewportWidth.value * .45), viewportWidth.value - 360))
const style = computed(() => mobile.value
  ? { top: `${viewportTop.value}px`, height: `${viewportHeight.value}px` }
  : { width: maximized.value ? '100%' : `${panelWidth.value}px` })
let observer: ResizeObserver | undefined
let themeObserver: MutationObserver | undefined
let timer: ReturnType<typeof setInterval> | undefined
let activeTerm: Term | undefined
let connection: ReturnType<typeof configureTtydClient>
let previousDocument: Document | null | undefined
let frameCleanup: (() => void) | undefined
let dragCleanup: (() => void) | undefined

function term() { return (frame.value?.contentWindow as (Window & { term?: Term }) | null)?.term }
function fit() {
  if (!props.open) return
  requestAnimationFrame(() => {
    frame.value?.contentWindow?.dispatchEvent(new Event('resize'))
    if (frame.value?.clientWidth && frame.value.clientHeight) term()?.fit()
  })
}
function focus() { term()?.focus() }
function openExternal() { window.open('/api/terminal/', '_blank', 'noopener') }
function updateViewport() {
  mobile.value = isMobile()
  viewportWidth.value = window.innerWidth
  const viewport = window.visualViewport
  viewportTop.value = viewport?.offsetTop || 0
  viewportHeight.value = viewport?.height || window.innerHeight
  fit()
  nextTick(updateKeyFades)
}
function updateKeyFades() {
  const row = keyRow.value
  fadeLeft.value = !!row && row.scrollLeft > 1
  fadeRight.value = !!row && row.scrollLeft + row.clientWidth < row.scrollWidth - 1
}
function applyAppearance() {
  const terminal = term()
  const doc = frame.value?.contentDocument
  if (!terminal || !doc) return
  const css = getComputedStyle(document.documentElement)
  const dark = document.documentElement.classList.contains('dark')
  const colour = (name: string) => css.getPropertyValue(name).trim()
  const background = dark ? colour('--background') : '#fafafa'
  terminal.options.fontFamily = 'ui-monospace, "SF Mono", "JetBrains Mono", Menlo, Consolas, monospace'
  terminal.options.fontSize = fontSize.value
  terminal.options.lineHeight = 1.2
  terminal.options.theme = {
    background, foreground: colour('--foreground'), cursor: colour('--primary'), selectionBackground: colour('--accent'),
    ...(dark ? {
      black: '#27272a', red: '#f87171', green: '#4ade80', yellow: '#facc15', blue: '#60a5fa', magenta: '#c084fc', cyan: '#22d3ee', white: '#d4d4d8',
      brightBlack: '#71717a', brightRed: '#fca5a5', brightGreen: '#86efac', brightYellow: '#fde047', brightBlue: '#93c5fd', brightMagenta: '#d8b4fe', brightCyan: '#67e8f9', brightWhite: '#fafafa',
    } : {
      black: '#18181b', red: '#b91c1c', green: '#15803d', yellow: '#854d0e', blue: '#1d4ed8', magenta: '#7e22ce', cyan: '#0e7490', white: '#52525b',
      brightBlack: '#71717a', brightRed: '#dc2626', brightGreen: '#166534', brightYellow: '#92400e', brightBlue: '#2563eb', brightMagenta: '#9333ea', brightCyan: '#155e75', brightWhite: '#3f3f46',
    }),
  }
  let sheet = doc.getElementById('board-terminal-style') as HTMLStyleElement | null
  if (!sheet) { sheet = doc.createElement('style'); sheet.id = 'board-terminal-style'; doc.head.appendChild(sheet) }
  sheet.textContent = `html,body { background:${background}!important; } body { margin:0!important; } #terminal-container { box-sizing:border-box; padding:8px!important; width:100%!important; height:100%!important; } #terminal-container .terminal { padding:0!important; height:100%!important; } html,body,.xterm-viewport { scrollbar-width:none!important; } ::-webkit-scrollbar { display:none!important; }`
  fit()
}
function sendKey(key: string) {
  if (key === 'Ctrl' || key === 'Alt') modifiers.value = toggleTerminalModifier(modifiers.value, key)
  else {
    const result = consumeTerminalKey(modifiers.value, key)
    modifiers.value = result.modifiers
    const terminal = term()
    // The board's xterm predates public input(); paste() would bracket escape keys.
    if (terminal?.input) terminal.input(result.bytes, true)
    else terminal?._core.coreService.triggerDataEvent(result.bytes, true)
  }
  focus()
}
function bindTerminal() {
  const terminal = term()
  if (!terminal || terminal === activeTerm) return
  const doc = frame.value!.contentDocument!
  const configured = configureTtydClient(doc, terminal, () => {
    status.value = 'disconnected'
    showConnectionBar.value = true
    terminal.options.cursorBlink = false
    terminal.options.disableStdin = true
    terminal._core.coreService.isCursorHidden = true
    terminal.refresh(0, terminal.rows - 1)
  }, applyAppearance)
  if (!configured) return
  connection = configured
  activeTerm = terminal
  terminalReady.value = true
  terminal.attachCustomKeyEventHandler(event => {
    if (status.value !== 'connected') return false
    if (!modifiers.value.ctrl && !modifiers.value.alt) return true
    if (['Control', 'Alt', 'Shift', 'Meta'].includes(event.key)) return true
    if (event.type === 'keydown') { event.preventDefault(); sendKey(event.key) }
    return false
  })
  // Touch keyboards can send beforeinput without a corresponding keydown.
  const beforeInput = (event: Event) => {
    const input = event as InputEvent
    if (input.data && (modifiers.value.ctrl || modifiers.value.alt)) {
      input.preventDefault(); input.stopImmediatePropagation(); sendKey(input.data)
    }
  }
  doc.addEventListener('beforeinput', beforeInput, true)
  frameCleanup?.()
  frameCleanup = () => {
    doc.removeEventListener('beforeinput', beforeInput, true)
    configured.cleanup()
  }
  applyAppearance()
  focus()
}
function inspectConnection() {
  const doc = frame.value?.contentDocument
  if (!doc || doc === previousDocument) return
  bindTerminal()
  if (connection?.socket.readyState === WebSocket.CLOSED) {
    status.value = 'disconnected'
    showConnectionBar.value = true
  } else if (connection?.socket.readyState === WebSocket.OPEN) {
    status.value = 'connected'
    showConnectionBar.value = false
  }
  host.value = doc.title.match(/([\w.-]+@[\w.-]+)/)?.[1] || ''
}
function reload() {
  status.value = 'connecting'
  frameCleanup?.()
  connection = undefined
  terminalReady.value = false
  previousDocument = frame.value?.contentDocument
  activeTerm = undefined
  modifiers.value = { ctrl: false, alt: false }
  if (frame.value) frame.value.src = '/api/terminal/'
}
function frameLoaded() { inspectConnection() }
function changeFont(delta: number) {
  fontSize.value = Math.min(20, Math.max(11, fontSize.value + delta))
  localStorage.setItem('terminalFontSize', String(fontSize.value))
  applyAppearance()
}
function toggleMaximized() { maximized.value = !maximized.value; emit('maximized', maximized.value); fit() }
function resetWidth() { width.value = 0; localStorage.removeItem('terminalPanelWidth'); fit() }
function startDrag(event: PointerEvent) {
  if (event.button !== 0) return
  event.preventDefault()
  const handle = event.currentTarget as HTMLElement
  handle.setPointerCapture(event.pointerId)
  const move = (e: PointerEvent) => { width.value = Math.min(Math.max(480, window.innerWidth - e.clientX), window.innerWidth - 360) }
  const end = () => { localStorage.setItem('terminalPanelWidth', String(panelWidth.value)); cleanup() }
  const cleanup = () => { handle.removeEventListener('pointermove', move); handle.removeEventListener('pointerup', end); handle.removeEventListener('pointercancel', end); handle.classList.remove('dragging'); dragCleanup = undefined }
  handle.classList.add('dragging')
  handle.addEventListener('pointermove', move)
  handle.addEventListener('pointerup', end)
  handle.addEventListener('pointercancel', end)
  dragCleanup = cleanup
}
watch(() => props.open, async open => {
  if (open) { mounted.value = true; await nextTick(); fit(); focus() }
  else { maximized.value = false; modifiers.value = { ctrl: false, alt: false }; emit('maximized', false) }
}, { immediate: true })
onMounted(() => {
  observer = new ResizeObserver(() => {
    emit('width', mobile.value ? 0 : panel.value?.getBoundingClientRect().width || 0)
    fit()
    updateKeyFades()
  })
  if (panel.value) observer.observe(panel.value)
  themeObserver = new MutationObserver(applyAppearance)
  themeObserver.observe(document.documentElement, { attributes: true, attributeFilter: ['class', 'style'] })
  timer = setInterval(inspectConnection, 150)
  window.addEventListener('resize', updateViewport)
  window.visualViewport?.addEventListener('resize', updateViewport)
  window.visualViewport?.addEventListener('scroll', updateViewport)
  updateViewport()
})
onBeforeUnmount(() => {
  observer?.disconnect(); themeObserver?.disconnect(); clearInterval(timer); frameCleanup?.(); dragCleanup?.()
  window.removeEventListener('resize', updateViewport)
  window.visualViewport?.removeEventListener('resize', updateViewport)
  window.visualViewport?.removeEventListener('scroll', updateViewport)
})
</script>

<template>
  <section v-show="open" ref="panel" data-board-terminal :style="style" class="board-terminal bg-background text-foreground" :class="{ 'board-terminal--mobile': mobile }" :aria-label="t('terminal.title')">
    <div v-if="!mobile && !maximized" class="terminal-resizer" role="separator" aria-orientation="vertical" :aria-label="t('terminal.resize')" @pointerdown="startDrag" @dblclick="resetWidth" />
    <header class="flex h-9 min-h-9 max-h-9 shrink-0 items-center justify-between gap-1 border-b px-2">
      <div class="flex min-w-0 flex-1 items-center gap-1.5 text-xs">
        <Terminal class="size-4 shrink-0" />
        <span class="shrink-0 font-semibold">{{ t('terminal.title') }}</span>
        <span v-if="host && !mobile" class="min-w-0 max-w-max flex-1 basis-0 truncate text-muted-foreground">· {{ host }}</span>
        <span class="size-1.5 shrink-0 rounded-full" :class="status === 'connected' ? 'bg-green-500' : status === 'connecting' ? 'bg-amber-500' : 'bg-red-500'" />
        <span data-terminal-status class="min-w-0 truncate text-xs text-muted-foreground">{{ t(`terminal.${status}`) }}</span>
      </div>
      <div class="flex shrink-0 items-center">
        <template v-if="!mobile">
          <Button variant="ghost" size="icon-sm" :disabled="fontSize <= 11" :aria-label="t('terminal.smaller')" :title="t('terminal.smaller')" @click="changeFont(-1)">A−</Button>
          <Button variant="ghost" size="icon-sm" :disabled="fontSize >= 20" :aria-label="t('terminal.larger')" :title="t('terminal.larger')" @click="changeFont(1)">A+</Button>
          <Button variant="ghost" size="icon-sm" :aria-label="t(maximized ? 'terminal.restore' : 'terminal.maximize')" :title="t(maximized ? 'terminal.restore' : 'terminal.maximize')" @click="toggleMaximized"><component :is="maximized ? Minimize2 : Maximize2" class="size-3.5" /></Button>
        </template>
        <Button variant="ghost" size="icon-sm" :aria-label="t('terminal.external')" :title="t('terminal.external')" @click="openExternal"><ExternalLink class="size-3.5" /></Button>
        <Button variant="ghost" size="icon-sm" :aria-label="t('terminal.close')" :title="t('terminal.close')" @click="emit('update:open', false)"><X class="size-3.5" /></Button>
      </div>
    </header>
    <div v-if="showConnectionBar" class="terminal-disconnected flex h-8 shrink-0 items-center justify-between gap-2 border-b px-2" role="status">
      <span class="flex min-w-0 items-center gap-1.5 text-sm"><AlertCircle class="size-4 shrink-0 text-destructive" /><span class="truncate">{{ t('terminal.ended') }}</span></span>
      <Button size="sm" variant="outline" class="h-7 shrink-0 gap-1.5" :disabled="status === 'connecting'" @click="reload"><RotateCw class="size-3.5" :class="status === 'connecting' && 'animate-spin'" />{{ t(status === 'connecting' ? 'terminal.connecting' : 'terminal.reconnect') }}</Button>
    </div>
    <iframe v-if="mounted" ref="frame" src="/api/terminal/" :title="t('terminal.title')" class="block min-h-0 w-full flex-1 border-0" :style="{ visibility: terminalReady ? 'visible' : 'hidden', opacity: showConnectionBar ? .5 : 1 }" allow="clipboard-read; clipboard-write" @load="frameLoaded" />
    <div v-if="mobile" class="terminal-key-strip relative shrink-0 border-t bg-background">
    <div ref="keyRow" class="terminal-keys flex overflow-x-auto px-2" :aria-label="t('terminal.keys')" @scroll="updateKeyFades">
      <button v-for="key in keys" :key="key" class="h-11 min-w-11 shrink-0 p-0.5" :aria-pressed="key === 'Ctrl' ? modifiers.ctrl : key === 'Alt' ? modifiers.alt : undefined" @pointerdown.prevent="sendKey(key)"><span class="flex h-10 items-center justify-center rounded text-sm" :class="(key === 'Ctrl' && modifiers.ctrl) || (key === 'Alt' && modifiers.alt) ? 'bg-accent text-accent-foreground' : 'bg-muted'">{{ key }}</span></button>
    </div>
    <span v-show="fadeLeft" class="terminal-key-fade terminal-key-fade--left" aria-hidden="true" />
    <span v-show="fadeRight" class="terminal-key-fade terminal-key-fade--right" aria-hidden="true" />
    </div>
  </section>
</template>

<style scoped>
.board-terminal { position:relative; display:flex; flex-direction:column; flex-shrink:0; min-width:0; height:100%; border-left:1px solid var(--border); }
.terminal-resizer { position:absolute; left:-3px; top:0; bottom:0; width:6px; z-index:2; cursor:col-resize; touch-action:none; }
.terminal-resizer:hover, .terminal-resizer.dragging { background:var(--primary); }
.board-terminal--mobile { position:fixed; left:0; width:100%; z-index:60; border-left:0; }
.terminal-disconnected { background:color-mix(in srgb, var(--destructive) 8%, transparent); border-color:color-mix(in srgb, var(--destructive) 20%, transparent); }
:global(.dark) .terminal-disconnected { background:color-mix(in srgb, var(--destructive) 15%, transparent); }
.terminal-keys { padding-bottom:env(safe-area-inset-bottom); scroll-snap-type:x proximity; scroll-padding-inline:8px; }
.terminal-keys button { scroll-snap-align:start; }
.terminal-key-fade { position:absolute; top:0; bottom:0; width:24px; pointer-events:none; }
.terminal-key-fade--left { left:0; background:linear-gradient(to right, var(--background), transparent); }
.terminal-key-fade--right { right:0; background:linear-gradient(to left, var(--background), transparent); }
</style>
