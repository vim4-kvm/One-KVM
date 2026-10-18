<script setup lang="ts">
import { computed, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { Button } from '@/components/ui/button'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import { ChevronDown, ChevronUp, Trash2 } from 'lucide-vue-next'
import { formatDetail } from './formatDetail'
import type { McpCallRecord } from '@/types/generated'
import { clientName } from './useMcp'

const props = defineProps<{ records: McpCallRecord[]; busy?: boolean }>()
defineEmits<{ clear: [] }>()
const { t, te } = useI18n()
const expanded = ref(false)
const visible = computed(() => expanded.value ? props.records : props.records.slice(0, 10))
function time(at: string) {
  return new Date(at).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit', hourCycle: 'h23' })
}
function content(record: McpCallRecord) {
  if (record.tool === 'type' && record.text != null) return `"${record.text.replace(/\r\n|\r|\n/g, '⏎')}"`
  return formatDetail(record.detail, t)
}
function failure(error: string | null) {
  if (/preempt|manual|taken over|took over/i.test(error ?? '')) return t('extensions.mcp.preempted')
  return t(/busy|another MCP action is still running/i.test(error ?? '') ? 'extensions.mcp.busy' : 'extensions.mcp.failed')
}
</script>

<template>
  <Card class="min-w-0">
    <CardHeader class="flex flex-row items-center justify-between gap-3">
      <CardTitle>{{ t('extensions.mcp.recentActivity') }}</CardTitle>
      <Button variant="ghost" size="sm" :disabled="busy || !records.length" @click="$emit('clear')"><Trash2 class="size-4" />{{ t('extensions.mcp.clear') }}</Button>
    </CardHeader>
    <CardContent class="min-w-0 space-y-2">
      <p v-if="!records.length" class="text-sm text-muted-foreground">{{ t('extensions.mcp.noActivity') }}</p>
      <ul v-else class="grid gap-x-3 gap-y-3 text-sm sm:grid-cols-[auto_auto_minmax(0,1fr)_minmax(0,8rem)_auto]">
        <li v-for="(record, index) in visible" :key="`${record.at}-${index}`" class="grid min-w-0 grid-cols-[auto_minmax(0,1fr)] items-baseline gap-x-3 gap-y-1 sm:col-span-5 sm:grid-cols-subgrid">
          <time class="hidden tabular-nums text-xs text-muted-foreground sm:block" :datetime="record.at">{{ time(record.at) }}</time>
          <span>{{ te(`extensions.mcp.tools.${record.tool}`) ? t(`extensions.mcp.tools.${record.tool}`) : record.tool }}</span>
          <span class="min-w-0 truncate" :class="['type', 'key', 'hold_key'].includes(record.tool) ? 'font-mono' : 'text-muted-foreground'" :title="content(record)">{{ content(record) }}</span>
          <span class="hidden truncate text-xs text-muted-foreground sm:block">{{ clientName(record.client, t('extensions.mcp.aiClient')) }}</span>
          <span class="hidden text-xs text-destructive sm:block" :title="record.error ?? undefined">{{ record.ok ? '' : failure(record.error) }}</span>
          <div class="col-span-2 flex min-w-0 gap-1 text-xs text-muted-foreground sm:hidden">
            <span class="truncate">{{ clientName(record.client, t('extensions.mcp.aiClient')) }}</span>
            <span>·</span><time class="shrink-0 tabular-nums" :datetime="record.at">{{ time(record.at) }}</time>
            <template v-if="!record.ok"><span>·</span><span class="text-destructive" :title="record.error ?? undefined">{{ failure(record.error) }}</span></template>
          </div>
        </li>
      </ul>
      <Button v-if="records.length > 10" variant="ghost" size="sm" class="pl-0 text-xs text-muted-foreground has-[>svg]:pl-0" @click="expanded = !expanded">
        {{ t(expanded ? 'extensions.mcp.showLess' : 'extensions.mcp.showAll', { count: records.length }) }}
        <ChevronUp v-if="expanded" class="size-3.5" /><ChevronDown v-else class="size-3.5" />
      </Button>
    </CardContent>
  </Card>
</template>
