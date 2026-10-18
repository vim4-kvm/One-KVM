<script setup lang="ts">
import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { RouterLink } from 'vue-router'
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover'
import { useWebSocket } from '@/composables/useWebSocket'
import { mcpApi } from '@/api/mcp'
import type { McpActivity } from '@/types/generated'
import { clientName } from './mcp/useMcp'
import { formatDetail } from './mcp/formatDetail'

const { t, te } = useI18n()
const socket = useWebSocket()
const activity = ref<McpActivity | null>(null)
const open = ref(false)
let revision = 0
let hoverTimer: ReturnType<typeof setTimeout> | undefined
const visible = computed(() => activity.value && activity.value.state !== 'idle')
const manual = computed(() => activity.value?.state === 'manual')
const label = computed(() => manual.value ? t('extensions.mcp.tookOver') : t('extensions.mcp.active', {
  client: clientName(activity.value?.client ?? null, t('extensions.mcp.aiClient')),
}))
const tool = computed(() => {
  const name = activity.value?.tool
  const label = name && te(`extensions.mcp.tools.${name}`) ? t(`extensions.mcp.tools.${name}`) : name
  return [label, formatDetail(activity.value?.detail, t)].filter(Boolean).join(' ')
})
function receive(next: McpActivity) {
  revision++
  activity.value = next
  if (next.state === 'idle') open.value = false
}
socket.on('mcp.activity', receive)
watch(socket.connected, async connected => {
  const started = ++revision
  activity.value = null
  open.value = false
  if (!connected) return
  try {
    const snapshot = await mcpApi.status()
    // A live event received during the GET is newer than the snapshot.
    if (started === revision) activity.value = snapshot.enabled ? snapshot.activity : null
  } catch { /* Live events remain authoritative; retry the snapshot on reconnect. */ }
}, { immediate: true })
function enter() {
  clearTimeout(hoverTimer)
  open.value = true
}
function leave() {
  hoverTimer = setTimeout(() => { open.value = false }, 150)
}
onBeforeUnmount(() => {
  revision++
  clearTimeout(hoverTimer)
  socket.off('mcp.activity', receive)
})
</script>

<template>
  <Transition leave-active-class="transition-opacity duration-300" leave-to-class="opacity-0">
    <div v-if="visible" class="inline-flex shrink-0">
      <Popover v-model:open="open">
        <PopoverTrigger as-child>
          <button type="button" :aria-label="label" class="inline-flex h-7 max-w-64 items-center gap-1.5 rounded-full px-2.5 text-xs font-medium ring-1 ring-inset focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring"
            :class="manual ? 'bg-muted text-muted-foreground ring-border' : 'bg-amber-500/10 text-amber-600 ring-amber-500/30 dark:text-amber-400'"
            @pointerenter="event => { if (event.pointerType === 'mouse') enter() }" @pointerleave="leave">
            <span class="size-2 shrink-0 rounded-full" :class="manual ? 'bg-muted-foreground' : 'animate-pulse bg-amber-500'" />
            <span class="hidden truncate sm:inline">{{ label }}</span><span class="sm:hidden">{{ t('extensions.mcp.ai') }}</span>
          </button>
        </PopoverTrigger>
        <PopoverContent class="max-w-[calc(100vw-2rem)] space-y-2 break-words text-sm" @pointerenter="enter" @pointerleave="leave">
          <p v-if="manual">{{ t('extensions.mcp.tookOver') }}</p>
          <template v-else>
            <p v-if="tool">{{ t('extensions.mcp.current', { tool }) }}</p>
            <p>{{ t('extensions.mcp.takeOverHint') }}</p>
          </template>
          <RouterLink class="inline-block underline" to="/settings?tab=ext-mcp">{{ t('extensions.mcp.settings') }}</RouterLink>
        </PopoverContent>
      </Popover>
    </div>
  </Transition>
</template>
