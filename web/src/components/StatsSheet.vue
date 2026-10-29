<script setup lang="ts">
import { ref, watch, onMounted, onUnmounted, nextTick, computed } from 'vue'
import { useI18n } from 'vue-i18n'
import uPlot from 'uplot'
import 'uplot/dist/uPlot.min.css'
import { BarChart3, X } from 'lucide-vue-next'
import { Button } from '@/components/ui/button'
import { ScrollArea } from '@/components/ui/scroll-area'
import { Badge } from '@/components/ui/badge'
import { Item, ItemContent, ItemDescription, ItemTitle } from '@/components/ui/item'
import { Card } from '@/components/ui/card'
import type { WebRTCStats } from '@/composables/useWebRTC'
import { formatFpsValue } from '@/lib/fps'
import { formatMbps, formatMs, formatPct, type ConnectionSample } from '@/lib/connectionStats'

const { t } = useI18n()

const props = defineProps<{
  open: boolean
  webrtcStats?: WebRTCStats
  /** Shared rolling samples from useWebRTC, oldest first; timestamps come from the stats reports. */
  samples: ConnectionSample[]
}>()

const emit = defineEmits<{
  (e: 'update:open', value: boolean): void
}>()

type ChartId = 'bitrate' | 'fps' | 'rtt' | 'processing' | 'loss'
interface ChartDef { id: ChartId; title: string; unit: string; value: (s: ConnectionSample) => number; format: (v: number) => string; hint?: string }

const charts = computed<ChartDef[]>(() => [
  { id: 'bitrate', title: t('stats.bitrate'), unit: 'Mbps', value: s => s.bitrateMbps, format: formatMbps },
  { id: 'fps', title: t('stats.frameRate'), unit: 'fps', value: s => s.fps, format: v => formatFpsValue(Math.round(v)) },
  { id: 'rtt', title: t('stats.latency'), unit: 'ms', value: s => s.rttMs, format: formatMs, hint: t('stats.rttHint') },
  { id: 'processing', title: t('stats.processing'), unit: 'ms', value: s => s.processingMs, format: formatMs, hint: t('stats.processingHint') },
  { id: 'loss', title: t('stats.packetLoss'), unit: '%', value: s => s.lossPct, format: formatPct },
])

const chartEls: Partial<Record<ChartId, HTMLDivElement>> = {}
const plots: Partial<Record<ChartId, uPlot>> = {}
const setChartEl = (id: ChartId) => (el: unknown) => { if (el) chartEls[id] = el as HTMLDivElement; else delete chartEls[id] }

const chartColors = {
  line: '#3b82f6',
  fill: 'rgba(59, 130, 246, 0.1)',
  grid: 'rgba(148, 163, 184, 0.1)',
  axis: '#64748b',
}

const formatClock = (seconds: number) =>
  new Date(seconds * 1000).toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit', second: '2-digit', hour12: false })

const activeTooltip = ref({ chartId: '', time: '', value: '', left: 0, top: 0, visible: false })

function tooltipPlugin(def: ChartDef): uPlot.Plugin {
  return {
    hooks: {
      setCursor: [(u) => {
        const idx = u.cursor.idx
        const ts = idx == null ? undefined : u.data[0]?.[idx]
        const val = idx == null ? undefined : u.data[1]?.[idx]
        if (ts == null || val == null || u.cursor.left == null || u.cursor.top == null) return
        activeTooltip.value = { chartId: def.id, time: formatClock(ts), value: `${def.format(val)} ${def.unit}`, left: u.cursor.left, top: u.cursor.top, visible: true }
      }],
      ready: [(u) => {
        u.over.addEventListener('mouseleave', () => {
          if (activeTooltip.value.chartId === def.id) activeTooltip.value.visible = false
        })
      }],
    },
  }
}

function chartOptions(container: HTMLElement, def: ChartDef): uPlot.Options {
  return {
    width: container.clientWidth || 300,
    height: 96,
    cursor: { show: true, x: true, y: false, drag: { x: false, y: false } },
    legend: { show: false },
    scales: {
      x: { time: false },
      // Zero-based so a steady link does not look noisy at magnified scale.
      y: { auto: true, range: (_u, _min, max) => [0, max > 0 ? max * 1.15 : 1] },
    },
    axes: [
      { show: true, stroke: chartColors.axis, grid: { show: false }, ticks: { show: false }, gap: 4, size: 20, values: (_, splits) => splits.map(formatClock), font: '10px system-ui' },
      { show: true, side: 1, stroke: chartColors.axis, size: 52, gap: 6, grid: { stroke: chartColors.grid, width: 1 }, values: (_, splits) => splits.map(v => def.format(v)), font: '10px system-ui' },
    ],
    series: [{}, { stroke: chartColors.line, width: 1.5, fill: chartColors.fill }],
    plugins: [tooltipPlugin(def)],
  }
}

function chartData(def: ChartDef): uPlot.AlignedData {
  return [props.samples.map(s => s.at / 1000), props.samples.map(def.value)]
}

function initCharts() {
  if (!props.open) return
  nextTick(() => {
    for (const def of charts.value) {
      const el = chartEls[def.id]
      if (el && !plots[def.id]) plots[def.id] = new uPlot(chartOptions(el, def), chartData(def), el)
    }
  })
}

function destroyCharts() {
  for (const id of Object.keys(plots) as ChartId[]) {
    plots[id]?.destroy()
    delete plots[id]
  }
}

watch(() => props.samples, () => {
  if (!props.open) return
  for (const def of charts.value) plots[def.id]?.setData(chartData(def))
})

const latest = computed(() => props.samples[props.samples.length - 1])

function formatCandidateType(type: string): string {
  const typeMap: Record<string, string> = {
    host: 'Host (Local)',
    srflx: 'STUN (NAT)',
    prflx: 'Peer Reflexive',
    relay: 'TURN Relay',
    unknown: '-',
  }
  return typeMap[type] || type
}

const connection = computed(() => {
  const s = props.webrtcStats
  return {
    resolution: s?.frameWidth && s.frameHeight ? `${s.frameWidth}×${s.frameHeight}` : '-',
    isRelay: s?.isRelay ?? false,
    transport: (s?.transportProtocol || '-').toUpperCase(),
    localType: formatCandidateType(s?.localCandidateType || 'unknown'),
    remoteType: formatCandidateType(s?.remoteCandidateType || 'unknown'),
  }
})

const receiverItems = computed(() => {
  const s = latest.value
  const w = props.webrtcStats
  return [
    { label: t('stats.jitterBuffer'), value: `${formatMs(s?.jitterBufferMs ?? 0)} ms` },
    { label: t('stats.decode'), value: `${formatMs(s?.decodeMs ?? 0)} ms` },
    { label: t('stats.jitter'), value: `${formatMs(s?.jitterMs ?? 0)} ms` },
    { label: t('stats.droppedFrames'), value: String(w?.framesDropped ?? 0) },
    { label: t('stats.freezes'), value: String(w?.freezeCount ?? 0) },
    { label: t('stats.packetsLost'), value: String(w?.packetsLost ?? 0) },
    { label: 'NACK / PLI', value: `${w?.nackCount ?? 0} / ${w?.pliCount ?? 0}` },
    { label: t('stats.keyFrames'), value: String(w?.keyFramesDecoded ?? 0) },
  ]
})

watch(() => props.open, (isOpen) => {
  if (isOpen) setTimeout(initCharts, 150)
  else destroyCharts()
})

function handleResize() {
  if (!props.open) return
  destroyCharts()
  setTimeout(initCharts, 50)
}

onMounted(() => {
  window.addEventListener('resize', handleResize)
  if (props.open) initCharts()
})

onUnmounted(() => {
  window.removeEventListener('resize', handleResize)
  destroyCharts()
})
</script>

<template>
  <section v-show="props.open" data-stats-panel class="stats-panel bg-background text-foreground" :aria-label="t('stats.title')">
      <header class="flex h-9 min-h-9 shrink-0 items-center justify-between gap-1 border-b pl-3 pr-2">
        <div class="flex min-w-0 items-center gap-1.5 text-xs">
          <BarChart3 class="size-4 shrink-0" />
          <span class="truncate font-semibold">{{ t('stats.title') }}</span>
          <Badge variant="secondary" class="h-5 px-1.5 text-[10px]">WebRTC</Badge>
        </div>
        <Button variant="ghost" size="icon-sm" :aria-label="t('stats.close')" :title="t('stats.close')" @click="emit('update:open', false)"><X class="size-3.5" /></Button>
      </header>

      <ScrollArea class="min-h-0 flex-1">
        <div class="px-4 py-4 space-y-5">
          <p class="text-xs text-muted-foreground">{{ t('stats.sampling') }}</p>

          <div v-for="def in charts" :key="def.id" class="space-y-1.5" :data-stats-chart="def.id">
            <div class="flex items-baseline justify-between gap-2">
              <h4 class="text-sm font-medium" :title="def.hint">{{ def.title }}</h4>
              <span class="text-xs tabular-nums text-muted-foreground">{{ latest ? def.format(def.value(latest)) : '-' }} {{ def.unit }}</span>
            </div>
            <Card class="relative gap-0 p-2 shadow-none">
              <div :ref="setChartEl(def.id)" class="w-full" />
              <div
                v-if="activeTooltip.visible && activeTooltip.chartId === def.id"
                class="chart-tooltip"
                :style="{ left: `${activeTooltip.left + 60}px`, top: `${activeTooltip.top - 40}px` }"
              >
                <div class="text-xs font-medium">{{ activeTooltip.time }}</div>
                <div class="text-xs text-info tabular-nums">{{ activeTooltip.value }}</div>
              </div>
            </Card>
            <p v-if="def.hint" class="text-[11px] leading-4 text-muted-foreground">{{ def.hint }}</p>
          </div>

          <div class="space-y-3 border-t pt-3">
            <h4 class="text-sm font-medium">{{ t('stats.receiver') }}</h4>
            <div class="grid grid-cols-2 gap-3">
              <Item v-for="item in receiverItems" :key="item.label" variant="muted" size="sm"><ItemContent><ItemDescription>{{ item.label }}</ItemDescription><ItemTitle class="tabular-nums">{{ item.value }}</ItemTitle></ItemContent></Item>
            </div>
          </div>

          <div class="space-y-3 border-t pt-3">
            <h4 class="text-sm font-medium">{{ t('stats.connection') }}</h4>
            <div class="grid grid-cols-2 gap-3">
              <Item variant="muted" size="sm"><ItemContent><ItemDescription>{{ t('stats.resolution') }}</ItemDescription><ItemTitle>{{ connection.resolution }}</ItemTitle></ItemContent></Item>
              <Item variant="muted" size="sm"><ItemContent><ItemDescription>{{ t('stats.connectionType') }}</ItemDescription><ItemTitle class="flex items-center gap-1.5">
                  <span :class="['inline-block size-2 rounded-full', connection.isRelay ? 'bg-warning' : 'bg-status-active']" />
                  {{ connection.isRelay ? t('stats.relay') : t('stats.p2p') }}
                </ItemTitle></ItemContent></Item>
              <Item variant="muted" size="sm"><ItemContent><ItemDescription>{{ t('stats.transport') }}</ItemDescription><ItemTitle>{{ connection.transport }}</ItemTitle></ItemContent></Item>
              <Item variant="muted" size="sm"><ItemContent><ItemDescription>{{ t('stats.localCandidate') }}</ItemDescription><ItemTitle>{{ connection.localType }}</ItemTitle></ItemContent></Item>
              <Item variant="muted" size="sm"><ItemContent><ItemDescription>{{ t('stats.remoteCandidate') }}</ItemDescription><ItemTitle>{{ connection.remoteType }}</ItemTitle></ItemContent></Item>
            </div>
          </div>
        </div>
      </ScrollArea>
  </section>
</template>

<style scoped>
/* Docked beside the video like the board terminal, so the target stays operable. */
.stats-panel { position: relative; display: flex; flex-direction: column; flex-shrink: 0; width: 400px; max-width: calc(100vw - 360px); height: 100%; border-left: 1px solid var(--border); }
@media (max-width: 767px) {
  .stats-panel { position: fixed; inset: 0; z-index: 60; width: 100%; max-width: none; border-left: 0; }
}
</style>

<style>
/* Override uPlot styles for dark mode */
.dark .u-wrap {
  background: transparent !important;
}

.dark .u-over {
  background: transparent !important;
}

/* Chart cursor line */
.u-cursor-x {
  border-right: 1px dashed #64748b !important;
}

.u-cursor-y {
  display: none !important;
}

/* Chart tooltip */
.chart-tooltip {
  position: absolute;
  z-index: 50;
  pointer-events: none;
  padding: 6px 10px;
  border-radius: 6px;
  background: rgba(15, 23, 42, 0.9);
  color: white;
  box-shadow: 0 4px 6px -1px rgba(0, 0, 0, 0.1);
  white-space: nowrap;
  transform: translateX(-50%);
}

.dark .chart-tooltip {
  background: rgba(30, 41, 59, 0.95);
  border: 1px solid rgba(71, 85, 105, 0.5);
}
</style>
