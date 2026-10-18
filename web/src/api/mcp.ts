import { request } from './request'
import type { McpCallDetail, McpConfigUpdate, McpKey, McpStatus } from '@/types/generated'

const quiet = { toastOnError: false }
export const mcpApi = {
  detail: (id: number, signal: AbortSignal) => request<McpCallDetail>(`/mcp/activity/${id}`, { signal, cache: 'no-store' }, quiet),
  clearActivity: () => request<McpStatus>('/mcp/activity', { method: 'DELETE' }, quiet),
  status: () => request<McpStatus>('/mcp/status', { cache: 'no-store' }, quiet),
  patchConfig: (patch: McpConfigUpdate) => request<McpStatus>('/mcp/config', {
    method: 'PATCH', body: JSON.stringify(patch),
  }, quiet),
  getKey: () => request<McpKey>('/mcp/key', { cache: 'no-store' }, quiet),
  regenerateKey: () => request<McpKey>('/mcp/key/regenerate', { method: 'POST' }, quiet),
}
