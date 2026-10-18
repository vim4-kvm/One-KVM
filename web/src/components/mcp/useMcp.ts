import { ref } from 'vue'
import { mcpApi } from '@/api/mcp'
import type { McpConfigUpdate, McpStatus } from '@/types/generated'
import i18n from '@/i18n'

export function useMcp() {
  const response = ref<McpStatus | null>(null)
  const busy = ref(false)
  const stale = ref(false)
  let revision = 0
  let polling = false

  async function refresh() {
    if (busy.value || polling) return
    polling = true
    const started = revision
    try {
      const next = await mcpApi.status()
      if (started === revision && !busy.value) {
        response.value = next
        stale.value = false
      }
    } catch {
      if (started === revision) stale.value = true
    } finally {
      polling = false
    }
  }

  async function mutate<T>(action: () => Promise<T>): Promise<T> {
    if (busy.value) throw new Error(String(i18n.global.t('extensions.mcp.busy')))
    busy.value = true
    revision++
    try {
      return await action()
    } finally {
      busy.value = false
    }
  }

  const save = (patch: McpConfigUpdate) => mutate(async () => {
    response.value = await mcpApi.patchConfig(patch)
    stale.value = false
  })
  const regenerate = () => mutate(async () => {
    const key = await mcpApi.regenerateKey()
    // Rotation succeeded even if the subsequent status read fails.
    try {
      response.value = await mcpApi.status()
      stale.value = false
    } catch {
      stale.value = true
    }
    return key.api_key
  })
  const clearActivity = () => mutate(async () => {
    response.value = await mcpApi.clearActivity()
    stale.value = false
  })
  return { response, busy, stale, refresh, save, regenerate, clearActivity }
}
export type McpController = ReturnType<typeof useMcp>

export function clientName(client: string | null, fallback: string): string {
  if (!client) return fallback
  if (client === 'claude-code') return 'Claude Code'
  if (client.startsWith('codex')) return 'Codex'
  if (client === 'claude-ai') return 'Claude'
  return client
}
