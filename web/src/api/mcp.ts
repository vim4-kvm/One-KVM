import { request } from './request'
import type { McpConfigUpdate, McpKey, McpStatus } from '@/types/generated'

const quiet = { toastOnError: false }
export const mcpApi = {
  status: () => request<McpStatus>('/mcp/status', { cache: 'no-store' }, quiet),
  patchConfig: (patch: McpConfigUpdate) => request<McpStatus>('/mcp/config', {
    method: 'PATCH', body: JSON.stringify(patch),
  }, quiet),
  getKey: () => request<McpKey>('/mcp/key', { cache: 'no-store' }, quiet),
  regenerateKey: () => request<McpKey>('/mcp/key/regenerate', { method: 'POST' }, quiet),
}
