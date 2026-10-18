<script setup lang="ts">
import { computed, ref, watch, useId } from 'vue'
import { useMediaQuery, useElementSize } from '@vueuse/core'
import { useI18n } from 'vue-i18n'
import { Dialog, DialogContent, DialogTitle, DialogDescription } from '@/components/ui/dialog'
import { Sheet, SheetContent } from '@/components/ui/sheet'
import { Button } from '@/components/ui/button'
import { mcpApi } from '@/api/mcp'
import { ApiError } from '@/api/request'
import type { McpCallDetail, McpCallRecord } from '@/types/generated'
import { clientName } from './useMcp'

const selected = defineModel<number | null>({ required: true })
const props = defineProps<{ records: McpCallRecord[] }>()
const { t, te } = useI18n()
const mobile = useMediaQuery('(max-width: 639px)')
const detail = ref<McpCallDetail | null>(null)
const base = ref<McpCallDetail | null>(null)
const loading = ref(false)
const error = ref('')
const imageFailed = ref(false)
const imageBox = ref<HTMLElement>()
const { width } = useElementSize(imageBox)
const arrowId = useId()
const index = computed(() => props.records.findIndex(record => record.id === selected.value))
const record = computed(() => detail.value?.record)
const imageRecord = computed(() => record.value?.based_on != null ? base.value?.record : record.value)
const imageUrl = computed(() => imageRecord.value?.has_image ? `/api/mcp/activity/${imageRecord.value.id}/image` : null)
const expectsImage = computed(() => record.value && (record.value.based_on != null || ['screenshot', 'zoom'].includes(record.value.tool) && record.value.ok))
const screen = computed(() => base.value?.screen)
const marker = computed(() => point(detail.value?.arguments.coordinate) ?? detail.value?.cursor)
const start = computed(() => point(detail.value?.arguments.start_coordinate))
const radius = computed(() => 10 * (screen.value?.[0] ?? 1) / Math.max(width.value, 1))
const scrollEnd = computed(() => {
  if (!marker.value) return null
  const [x, y] = marker.value
  const distance = radius.value * 3
  switch (detail.value?.arguments.scroll_direction) {
    case 'up': return [x, y - distance]
    case 'down': return [x, y + distance]
    case 'left': return [x - distance, y]
    case 'right': return [x + distance, y]
    default: return null
  }
})
function point(value: unknown): [number, number] | null {
  return Array.isArray(value) && value.length === 2 && value.every(v => typeof v === 'number') ? value as [number, number] : null
}
function time(at: string) {
  return new Date(at).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23' })
}
function valueText(key: string, value: unknown) {
  const coordinate = key.endsWith('coordinate') ? point(value) : null
  return coordinate ? `(${coordinate.join(', ')})` : typeof value === 'string' ? value : JSON.stringify(value)
}
const resultLabel = computed(() => {
  if (record.value?.ok) return t('extensions.mcp.success')
  const message = record.value?.error ?? ''
  return t(`extensions.mcp.${/preempt|manual|taken over|took over/i.test(message) ? 'preempted' : /busy|another MCP action is still running/i.test(message) ? 'busy' : 'failed'}`)
})
function navigate(offset: number) {
  const next = props.records[index.value + offset]
  if (index.value >= 0 && next) selected.value = next.id
}
function keydown(event: KeyboardEvent) {
  if (event.key === 'ArrowLeft' || event.key === 'ArrowRight') {
    event.preventDefault()
    navigate(event.key === 'ArrowLeft' ? -1 : 1)
  }
}
watch(selected, async (id, _, onCleanup) => {
  const controller = new AbortController()
  onCleanup(() => controller.abort())
  detail.value = null
  base.value = null
  error.value = ''
  imageFailed.value = false
  loading.value = id != null
  if (id == null) return
  try {
    const result = await mcpApi.detail(id, controller.signal)
    if (controller.signal.aborted) return
    detail.value = result
    if (result.record.based_on != null) {
      try {
        const reference = await mcpApi.detail(result.record.based_on, controller.signal)
        if (!controller.signal.aborted) base.value = reference
      } catch (e) {
        if (!(e instanceof ApiError && e.status === 404)) throw e
      }
    }
  } catch (e) {
    if (!controller.signal.aborted) error.value = t(e instanceof ApiError && e.status === 404 ? 'extensions.mcp.recordCleared' : 'extensions.mcp.detailFailed')
  } finally {
    if (!controller.signal.aborted) loading.value = false
  }
}, { immediate: true })
</script>

<template>
  <component :is="mobile ? Sheet : Dialog" :open="selected != null" @update:open="open => { if (!open) selected = null }">
    <component :is="mobile ? SheetContent : DialogContent" side="bottom" :class="mobile ? 'max-h-[95dvh] rounded-t-xl p-5' : 'sm:max-w-3xl max-h-[90dvh] flex flex-col'" @keydown="keydown">
      <div class="min-h-0 overflow-y-auto space-y-5 pr-1">
        <div class="flex items-start justify-between gap-3 pr-7">
          <div>
            <DialogTitle>{{ record ? (te(`extensions.mcp.tools.${record.tool}`) ? t(`extensions.mcp.tools.${record.tool}`) : record.tool) : t('extensions.mcp.activityDetail') }}</DialogTitle>
            <DialogDescription class="mt-1">{{ record ? `${time(record.at)} · ${clientName(record.client, t('extensions.mcp.aiClient'))} · ${record.duration_ms} ms` : t('extensions.mcp.activityDetail') }}</DialogDescription>
          </div>
          <span v-if="record" class="shrink-0 rounded-full px-2 py-1 text-xs" :class="record.ok ? 'bg-green-500/10 text-green-600' : 'bg-destructive/10 text-destructive'">{{ resultLabel }}</span>
        </div>
        <div v-if="loading" class="animate-pulse space-y-3" role="status" :aria-label="t('extensions.mcp.loadingDetail')"><div class="h-48 rounded bg-muted" /><div class="h-6 rounded bg-muted" /><div class="h-12 rounded bg-muted" /></div>
        <p v-else-if="error" class="text-sm text-muted-foreground">{{ error }}</p>
        <template v-else-if="detail && record">
          <div v-if="imageUrl && !imageFailed">
            <a :href="imageUrl" target="_blank" rel="noopener" :aria-label="t('extensions.mcp.openImage')">
              <div ref="imageBox" class="relative overflow-hidden rounded-lg border">
                <img :key="imageUrl" :src="imageUrl" class="block w-full h-auto" :alt="t('extensions.mcp.tools.screenshot')" @error="imageFailed = true" />
                <svg v-if="record.based_on != null && screen && marker" class="pointer-events-none absolute inset-0 size-full text-red-500" :viewBox="`0 0 ${screen[0]} ${screen[1]}`" fill="none" stroke="currentColor" stroke-width="2">
                  <defs><marker :id="arrowId" markerWidth="8" markerHeight="8" refX="7" refY="4" orient="auto"><path d="M0,0 L8,4 L0,8" fill="currentColor" stroke="none" /></marker></defs>
                  <template v-if="record.tool === 'left_click_drag' && start">
                    <circle :cx="start[0]" :cy="start[1]" :r="radius" vector-effect="non-scaling-stroke" />
                    <line :x1="start[0]" :y1="start[1]" :x2="marker[0]" :y2="marker[1]" :marker-end="`url(#${arrowId})`" vector-effect="non-scaling-stroke" />
                    <circle :cx="marker[0]" :cy="marker[1]" :r="radius / 2" fill="currentColor" stroke="none" />
                  </template>
                  <template v-else>
                    <circle :cx="marker[0]" :cy="marker[1]" :r="radius" vector-effect="non-scaling-stroke" />
                    <circle :cx="marker[0]" :cy="marker[1]" :r="radius / 5" fill="currentColor" stroke="none" />
                    <path :d="`M${marker[0] - radius * 1.5},${marker[1]} h${radius * 3} M${marker[0]},${marker[1] - radius * 1.5} v${radius * 3}`" stroke-width="1" vector-effect="non-scaling-stroke" />
                    <line v-if="scrollEnd" :x1="marker[0]" :y1="marker[1]" :x2="scrollEnd[0]" :y2="scrollEnd[1]" :marker-end="`url(#${arrowId})`" vector-effect="non-scaling-stroke" />
                  </template>
                </svg>
              </div>
            </a>
            <p v-if="base" class="mt-2 text-xs text-muted-foreground">{{ t('extensions.mcp.markerBasedOn', { time: time(base.record.at) }) }}</p>
            <p v-if="record.tool === 'zoom'" class="mt-2 text-xs text-muted-foreground">{{ t('extensions.mcp.zoomRegion', { region: JSON.stringify(detail.arguments.region) }) }}</p>
          </div>
          <p v-else-if="expectsImage" class="text-sm text-muted-foreground">{{ t('extensions.mcp.imageEvicted') }}</p>
          <section class="space-y-2"><h3 class="text-sm font-medium">{{ t('extensions.mcp.arguments') }}</h3>
            <dl class="space-y-2 text-sm"><div v-for="(value, key) in detail.arguments" :key="key" class="grid grid-cols-[max-content_minmax(0,1fr)] gap-3"><dt class="text-muted-foreground">{{ key }}</dt><dd class="min-w-0"><pre class="whitespace-pre-wrap break-words font-mono select-text">{{ valueText(key, value) }}</pre></dd></div></dl>
          </section>
          <section v-if="detail.result_text || record.error" class="space-y-2"><h3 class="text-sm font-medium">{{ t('extensions.mcp.returnValue') }}</h3><pre v-if="detail.result_text" class="rounded bg-muted p-3 text-sm font-mono whitespace-pre-wrap break-words select-text">{{ detail.result_text }}</pre><p v-if="record.error" class="text-sm text-destructive break-words">{{ record.error }}</p></section>
        </template>
      </div>
      <div class="flex shrink-0 justify-between gap-2 border-t pt-4">
        <div class="flex gap-2"><Button variant="outline" :disabled="index <= 0" @click="navigate(-1)">{{ t('extensions.mcp.previousRecord') }}</Button><Button variant="outline" :disabled="index < 0 || index >= records.length - 1" @click="navigate(1)">{{ t('extensions.mcp.nextRecord') }}</Button></div>
        <Button variant="outline" @click="selected = null">{{ t('extensions.mcp.closeDetail') }}</Button>
      </div>
    </component>
  </component>
</template>
