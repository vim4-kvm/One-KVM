import { computed, onBeforeUnmount, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { mcpApi } from '@/api/mcp'
import { useWebSocket } from '@/composables/useWebSocket'
import { clientName } from '@/components/mcp/useMcp'
import { formatDetail } from '@/components/mcp/formatDetail'
import type { McpActivity } from '@/types/generated'
import type { ConnectionStatus, StatusDetail } from '@/components/StatusCard.vue'

export function useMcpActivity() {
  const { t, te } = useI18n()
  const socket = useWebSocket()
  const enabled = ref(false)
  const activity = ref<McpActivity | null>(null)
  const ready = ref(false)
  const credentialsError = ref<string | null>(null)
  let connectionRevision = 0
  let activityRevision = 0

  function receive(next: McpActivity) {
    activityRevision++
    activity.value = next
  }
  socket.on('mcp.activity', receive)
  watch(socket.connected, async connected => {
    const connection = ++connectionRevision
    const revision = activityRevision
    // Keep the last known enabled flag so reconnects do not shift the toolbar.
    ready.value = false
    if (!connected) return
    try {
      const snapshot = await mcpApi.status()
      if (connection !== connectionRevision) return
      enabled.value = snapshot.enabled
      credentialsError.value = snapshot.credentials_error
      if (revision === activityRevision) activity.value = snapshot.activity
      ready.value = true
    } catch { /* Retry the snapshot on reconnect; never present stale activity as live. */ }
  }, { immediate: true })
  onBeforeUnmount(() => {
    connectionRevision++
    socket.off('mcp.activity', receive)
  })

  const state = computed(() => socket.connected.value && ready.value
    ? credentialsError.value ? 'error' : activity.value?.state : undefined)
  const status = computed<ConnectionStatus>(() => {
    switch (state.value) {
      case 'idle': return 'disconnected'
      case 'active': return 'active-ai'
      case 'manual': return 'manual'
      case 'error': return 'error'
      default: return 'connecting'
    }
  })
  const quickInfo = computed(() => {
    switch (state.value) {
      case 'idle': return t('extensions.mcp.standby')
      case 'active': return t('extensions.mcp.active', {
        client: clientName(activity.value?.client ?? null, t('extensions.mcp.aiClient')),
      })
      case 'manual': return t('extensions.mcp.tookOver')
      case 'error': return t('extensions.mcp.keyUnavailable')
      default: return t('statusCard.connecting')
    }
  })
  const details = computed<StatusDetail[]>(() => {
    const name = activity.value?.tool
    const tool = name && te(`extensions.mcp.tools.${name}`) ? t(`extensions.mcp.tools.${name}`) : name
    return [
      { label: t('extensions.mcp.state'), value: quickInfo.value },
      { label: t('extensions.mcp.client'), value: activity.value?.client ? clientName(activity.value.client, '—') : '—' },
      { label: t('extensions.mcp.currentOperation'), value: state.value === 'active'
        ? [tool, formatDetail(activity.value?.detail, t)].filter(Boolean).join(' ') || '—' : '—' },
    ]
  })
  const hint = computed(() => state.value === 'active' ? t('extensions.mcp.takeOverHint') : undefined)
  return { enabled, status, quickInfo, details, hint }
}
