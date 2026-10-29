<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { ChevronDown, ChevronUp, Maximize2, X } from 'lucide-vue-next'
import type { WebRTCStats } from '@/composables/useWebRTC'
import { formatFpsValue } from '@/lib/fps'
import {
  fpsLevel,
  formatMbps,
  formatMs,
  formatPct,
  lossLevel,
  nearestCorner,
  rttLevel,
  sparklinePoints,
  type ConnectionSample,
  type HudCorner,
  type StatsLevel,
} from '@/lib/connectionStats'

const props = defineProps<{
  webrtcStats?: WebRTCStats
  /** Shared rolling samples from useWebRTC, oldest first. */
  samples: ConnectionSample[]
  corner: HudCorner
  collapsed: boolean
  /** Keeps the card below floating toolbars that share the top edge. */
  topInset: number
}>()
const emit = defineEmits<{
  (e: 'update:corner', value: HudCorner): void
  (e: 'update:collapsed', value: boolean): void
  (e: 'details'): void
  (e: 'close'): void
}>()
const { t } = useI18n()

/** About 30 s at the 2 Hz refresh used while the card is open. */
const HISTORY = 60
const SPARK_W = 72
const SPARK_H = 16
const recent = computed(() => props.samples.slice(-HISTORY))
const latest = computed(() => recent.value[recent.value.length - 1])
const series = (key: keyof ConnectionSample) => recent.value.map(s => s[key])
const peakFps = computed(() => Math.max(0, ...series('fps')))

interface Row { id: string; label: string; value: string; points: string; level: StatsLevel; title?: string }
const rows = computed<Row[]>(() => {
  const s = latest.value
  const points = (key: keyof ConnectionSample) => sparklinePoints(series(key), SPARK_W, SPARK_H)
  return [
    { id: 'fps', label: t('stats.frameRate'), value: `${formatFpsValue(Math.round(s?.fps ?? 0))} fps`, points: points('fps'), level: fpsLevel(s?.fps ?? 0, peakFps.value), title: `${t('stats.droppedFrames')} ${props.webrtcStats?.framesDropped ?? 0}` },
    { id: 'bitrate', label: t('stats.bitrate'), value: `${formatMbps(s?.bitrateMbps ?? 0)} Mbps`, points: points('bitrateMbps'), level: 'ok' },
    { id: 'rtt', label: t('stats.latency'), value: `${formatMs(s?.rttMs ?? 0)} ms`, points: points('rttMs'), level: rttLevel(s?.rttMs ?? 0), title: `${t('stats.rttHint')} · ${t('stats.jitter')} ${formatMs(s?.jitterMs ?? 0)} ms` },
    { id: 'processing', label: t('stats.processing'), value: `${formatMs(s?.processingMs ?? 0)} ms`, points: points('processingMs'), level: 'ok', title: `${t('stats.jitterBuffer')} ${formatMs(s?.jitterBufferMs ?? 0)} ms · ${t('stats.decode')} ${formatMs(s?.decodeMs ?? 0)} ms` },
    { id: 'loss', label: t('stats.packetLoss'), value: `${formatPct(s?.lossPct ?? 0)} %`, points: points('lossPct'), level: lossLevel(s?.lossPct ?? 0), title: `${t('stats.packetsLost')} ${props.webrtcStats?.packetsLost ?? 0}` },
  ]
})
const worstLevel = computed<StatsLevel>(() => rows.value.some(r => r.level === 'bad') ? 'bad' : rows.value.some(r => r.level === 'warning') ? 'warning' : 'ok')
const summaryLine = computed(() => rows.value.filter(r => r.id !== 'processing').map(r => r.value.replace(' ', '')).join(' · '))
const connectionLine = computed(() => {
  const s = props.webrtcStats
  if (!s) return '-'
  const size = s.frameWidth && s.frameHeight ? `${s.frameWidth}×${s.frameHeight}` : '-'
  const route = s.isRelay ? t('stats.relay') : t('stats.p2p')
  const transport = s.transportProtocol ? `/${s.transportProtocol.toUpperCase()}` : ''
  return `${size} · ${route}${transport}`
})

const card = ref<HTMLElement>()
const dragOffset = ref<{ x: number; y: number } | null>(null)
const settling = ref(false)
let dragCleanup: (() => void) | undefined

function startDrag(event: PointerEvent) {
  if (event.button !== 0 || (event.target as HTMLElement).closest('button')) return
  const handle = event.currentTarget as HTMLElement
  const startX = event.clientX
  const startY = event.clientY
  handle.setPointerCapture(event.pointerId)
  dragOffset.value = { x: 0, y: 0 }
  const move = (e: PointerEvent) => { dragOffset.value = { x: e.clientX - startX, y: e.clientY - startY } }
  const end = () => {
    const box = card.value?.getBoundingClientRect()
    const area = card.value?.parentElement?.getBoundingClientRect()
    if (box && area && dragOffset.value && Math.hypot(dragOffset.value.x, dragOffset.value.y) > 4) {
      emit('update:corner', nearestCorner(box.left + box.width / 2 - area.left, box.top + box.height / 2 - area.top, area.width, area.height))
    }
    settling.value = true
    dragOffset.value = null
    cleanup()
  }
  const cleanup = () => {
    handle.removeEventListener('pointermove', move)
    handle.removeEventListener('pointerup', end)
    handle.removeEventListener('pointercancel', end)
    dragCleanup = undefined
  }
  handle.addEventListener('pointermove', move)
  handle.addEventListener('pointerup', end)
  handle.addEventListener('pointercancel', end)
  dragCleanup = cleanup
}

const cardStyle = computed(() => ({
  '--hud-top': `${props.topInset}px`,
  transform: dragOffset.value ? `translate(${dragOffset.value.x}px, ${dragOffset.value.y}px)` : undefined,
}))

watch(() => props.corner, () => { setTimeout(() => { settling.value = false }, 160) })

onBeforeUnmount(() => {
  dragCleanup?.()
})
</script>

<template>
  <section
    ref="card"
    data-stats-hud
    class="stats-hud"
    :class="[`stats-hud--${corner}`, { 'stats-hud--collapsed': collapsed, 'stats-hud--dragging': dragOffset, 'stats-hud--settling': settling }]"
    :style="cardStyle"
    :aria-label="t('stats.title')"
  >
    <header class="stats-hud__header" :title="t('stats.move')" @pointerdown="startDrag">
      <template v-if="collapsed">
        <span class="size-1.5 shrink-0 rounded-full" :class="`stats-dot--${worstLevel}`" />
        <span class="min-w-0 truncate tabular-nums">{{ summaryLine }}</span>
      </template>
      <span v-else class="min-w-0 truncate font-medium">{{ t('stats.title') }}</span>
      <span class="ml-auto flex shrink-0 items-center">
        <button type="button" class="stats-hud__btn" :aria-label="t(collapsed ? 'stats.expand' : 'stats.collapse')" :title="t(collapsed ? 'stats.expand' : 'stats.collapse')" @click="emit('update:collapsed', !collapsed)">
          <component :is="collapsed ? ChevronDown : ChevronUp" class="size-3.5" />
        </button>
        <button type="button" class="stats-hud__btn" :aria-label="t('stats.details')" :title="t('stats.details')" @click="emit('details')">
          <Maximize2 class="size-3.5" />
        </button>
        <button type="button" class="stats-hud__btn" :aria-label="t('stats.close')" :title="t('stats.close')" @click="emit('close')">
          <X class="size-3.5" />
        </button>
      </span>
    </header>
    <div v-if="!collapsed" class="px-3 pb-2.5">
      <p class="mb-1.5 truncate text-muted-foreground">{{ connectionLine }}</p>
      <dl class="space-y-1">
        <div v-for="row in rows" :key="row.id" class="stats-hud__row" :title="row.title" :data-level="row.level">
          <dt class="text-muted-foreground">{{ row.label }}</dt>
          <dd class="stats-hud__value tabular-nums">{{ row.value }}</dd>
          <svg :width="SPARK_W" :height="SPARK_H" :viewBox="`0 0 ${SPARK_W} ${SPARK_H}`" aria-hidden="true">
            <polyline :points="row.points" fill="none" stroke="currentColor" stroke-width="1" stroke-linejoin="round" />
          </svg>
        </div>
      </dl>
    </div>
  </section>
</template>

<style scoped>
.stats-hud {
  position: absolute; z-index: 35; width: 248px;
  font-size: 12px; line-height: 16px;
  border: 1px solid var(--border); border-radius: 12px;
  background: color-mix(in srgb, var(--background) 96%, transparent);
  backdrop-filter: blur(12px);
  box-shadow: 0 1px 3px rgb(0 0 0 / 10%);
  opacity: .92; transition: opacity 150ms;
  user-select: none;
}
.stats-hud:hover, .stats-hud:focus-within { opacity: 1; }
.stats-hud--collapsed { width: auto; max-width: min(320px, calc(100% - 16px)); border-radius: 9999px; }
.stats-hud--top-left { top: var(--hud-top); left: 8px; }
.stats-hud--top-right { top: var(--hud-top); right: 8px; }
.stats-hud--bottom-left { bottom: 8px; left: 8px; }
.stats-hud--bottom-right { bottom: 8px; right: 8px; }
.stats-hud--dragging { opacity: 1; cursor: grabbing; box-shadow: 0 8px 24px rgb(0 0 0 / 18%); }
.stats-hud--settling { transition: opacity 150ms, transform 150ms; }
.stats-hud__header {
  display: flex; align-items: center; gap: 6px; height: 32px; padding: 0 4px 0 12px;
  cursor: grab; touch-action: none;
}
.stats-hud--collapsed .stats-hud__header { height: 28px; }
.stats-hud__btn {
  display: inline-flex; align-items: center; justify-content: center;
  width: 24px; height: 24px; border-radius: 6px; color: var(--muted-foreground);
}
.stats-hud__btn:hover { background: var(--accent); color: var(--accent-foreground); }
.stats-hud__btn:focus-visible { outline: 2px solid var(--ring); outline-offset: -2px; }
.stats-hud__row { display: grid; grid-template-columns: 1fr auto 72px; align-items: center; column-gap: 8px; color: var(--chart-1, #3b82f6); }
.stats-hud__row dt { color: var(--muted-foreground); }
.stats-hud__value { min-width: 64px; text-align: right; color: var(--foreground); }
.stats-hud__row[data-level='warning'], .stats-hud__row[data-level='warning'] .stats-hud__value { color: var(--warning, #d97706); }
.stats-hud__row[data-level='bad'], .stats-hud__row[data-level='bad'] .stats-hud__value { color: var(--destructive); }
.stats-dot--ok { background: var(--status-active, #22c55e); }
.stats-dot--warning { background: var(--warning, #d97706); }
.stats-dot--bad { background: var(--destructive); }
@media (pointer: coarse) {
  .stats-hud__btn { width: 36px; height: 36px; }
  .stats-hud__header { height: 40px; }
  .stats-hud--collapsed .stats-hud__header { height: 36px; }
}
</style>
