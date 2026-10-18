<script setup lang="ts">
import { computed, onBeforeUnmount, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { Button } from '@/components/ui/button'
import { ChevronDown, ChevronUp } from 'lucide-vue-next'
import { formatDetail } from './formatDetail'
import type { McpCallRecord } from '@/types/generated'
import { clientName } from './useMcp'

const props = defineProps<{ records: McpCallRecord[] }>()
const { t, te } = useI18n()
const expanded = ref(false)
const now = ref(Date.now())
const timer = setInterval(() => { now.value = Date.now() }, 10000)
onBeforeUnmount(() => clearInterval(timer))
const visible = computed(() => props.records.slice(0, expanded.value ? 20 : 5))
function time(at: string) {
  const date = new Date(at)
  const seconds = Math.max(0, Math.floor((now.value - date.getTime()) / 1000))
  if (seconds < 10) return t('extensions.mcp.justNow')
  if (seconds < 60) return t('extensions.mcp.secondsAgo', { count: seconds })
  if (seconds < 3600) return t('extensions.mcp.minutesAgo', { count: Math.floor(seconds / 60) })
  return date.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', hour12: false })
}
function failure(error: string | null) {
  if (/preempt|manual|taken over|took over/i.test(error ?? '')) return t('extensions.mcp.preempted')
  return t(/busy|another MCP action is still running/i.test(error ?? '') ? 'extensions.mcp.busy' : 'extensions.mcp.failed')
}
</script>

<template>
  <section v-if="records.length" class="min-w-0 space-y-2">
    <h3 class="text-sm font-medium">{{ t('extensions.mcp.recentActivity') }}</h3>
    <ul class="space-y-2 text-sm">
      <li v-for="(record, index) in visible" :key="`${record.at}-${index}`" class="grid min-w-0 gap-1 sm:grid-cols-[minmax(0,1fr)_minmax(0,1fr)] sm:gap-3">
        <div class="min-w-0 break-words">
          <span class="mr-3 sm:inline-block sm:min-w-[5em]">{{ te(`extensions.mcp.tools.${record.tool}`) ? t(`extensions.mcp.tools.${record.tool}`) : record.tool }}</span>
          <span class="text-muted-foreground">{{ formatDetail(record.detail, t) }}</span>
          <span v-if="!record.ok" class="ml-2 text-destructive" :title="record.error ?? undefined">{{ failure(record.error) }}</span>
        </div>
        <div class="flex min-w-0 gap-2 text-xs text-muted-foreground sm:justify-end">
          <span class="min-w-0 break-words">{{ clientName(record.client, t('extensions.mcp.aiClient')) }}</span>
          <span class="sm:hidden">·</span>
          <time class="shrink-0 tabular-nums" :datetime="record.at" :title="record.at">{{ time(record.at) }}</time>
        </div>
      </li>
    </ul>
    <Button v-if="records.length > 5" variant="ghost" size="sm" class="pl-0 text-xs text-muted-foreground has-[>svg]:pl-0" @click="expanded = !expanded">
      {{ t(expanded ? 'extensions.mcp.showLess' : 'extensions.mcp.showAll', { count: Math.min(records.length, 20) }) }}
      <ChevronUp v-if="expanded" class="size-3.5" /><ChevronDown v-else class="size-3.5" />
    </Button>
  </section>
</template>
