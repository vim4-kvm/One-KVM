<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Check, Copy, Eye, EyeOff, Loader2, RefreshCw } from 'lucide-vue-next'
import { toast } from 'vue-sonner'
import { Card, CardContent, CardHeader, CardTitle } from '@/components/ui/card'
import SegmentedChoice from '@/components/SegmentedChoice.vue'
import { Button } from '@/components/ui/button'
import { Switch } from '@/components/ui/switch'
import { useClipboard } from '@/composables/useClipboard'
import { mcpApi } from '@/api/mcp'
import { clientName, type McpController } from './useMcp'
import McpClientSetup from './McpClientSetup.vue'
import McpActivityList from './McpActivityList.vue'

const props = defineProps<{ controller: McpController }>()
defineEmits<{ navigate: [section: 'hid'] }>()
const { response, busy, stale, refresh, save, regenerate, clearActivity } = props.controller
const { t } = useI18n()
const endpointClipboard = useClipboard()
const keyClipboard = useClipboard()
const error = ref('')
const fullKey = ref('')
const revealed = ref(false)
const keyBusy = ref(false)
const confirming = ref(false)
let keyRevision = 0
let disposed = false
const http = window.location.protocol === 'http:'
const sourceIsSmaller = computed(() => {
  const source = response.value?.source_size
  return source && Math.max(...source) < (response.value?.screenshot_long_edge ?? 1280)
})
async function setScreenshotSize(value: string) {
  error.value = ''
  try { await save({ screenshot_long_edge: Number(value) }) } catch (cause) { report(cause) }
}
async function clearRecords() {
  error.value = ''
  try { await clearActivity() } catch (cause) { report(cause) }
}
const endpoint = computed(() => window.location.origin + (response.value?.endpoint_path ?? '/api/mcp'))
const displayKey = computed(() => revealed.value ? fullKey.value : response.value?.api_key_preview ?? '••••••••')
const state = computed(() => response.value?.credentials_error ? 'error' : !response.value?.enabled ? 'off' : response.value.activity.state)
const statusText = computed(() => {
  if (state.value === 'active') return t('extensions.mcp.onActive', { client: clientName(response.value?.activity.client ?? null, t('extensions.mcp.aiClient')) })
  return t(`extensions.mcp.states.${state.value}`)
})
const stateClass = computed(() => ({
  off: 'border border-muted-foreground', idle: 'bg-emerald-500',
  active: 'bg-amber-500 animate-pulse', manual: 'bg-muted-foreground', error: 'bg-destructive',
})[state.value])
function hideKey() {
  keyRevision++
  fullKey.value = ''
  revealed.value = false
  confirming.value = false
}
watch(() => response.value?.enabled, enabled => { if (!enabled) hideKey() })
watch(() => response.value?.api_key_preview, (next, previous) => {
  // Another settings session may rotate the key while this card stays open.
  if (next !== previous && !busy.value) hideKey()
}, { flush: 'sync' })
onMounted(refresh)
onBeforeUnmount(() => { disposed = true; hideKey() })
function report(cause: unknown) {
  if (!disposed) error.value = cause instanceof Error ? cause.message : t('extensions.mcp.operationFailed')
}
async function getKey() {
  if (fullKey.value) return fullKey.value
  const started = keyRevision
  const result = await mcpApi.getKey()
  // A key read finishing after navigation or disabling must not reveal/copy it.
  if (disposed || started !== keyRevision) throw new Error(t('extensions.mcp.operationCancelled'))
  fullKey.value = result.api_key
  return result.api_key
}
async function keyAction(action: () => Promise<void>) {
  keyBusy.value = true
  error.value = ''
  try { await action() } catch (cause) { report(cause) }
  finally { keyBusy.value = false }
}
async function toggle(enabled: boolean) {
  error.value = ''
  if (!enabled) hideKey()
  try { await save({ enabled }) } catch (cause) { report(cause) }
}
async function toggleNaturalScrolling(host_natural_scrolling: boolean) {
  error.value = ''
  try { await save({ host_natural_scrolling }) } catch (cause) { report(cause) }
}
async function reveal() {
  if (revealed.value) { hideKey(); return }
  await keyAction(async () => { await getKey(); revealed.value = true })
}
async function copyKey() {
  await keyAction(async () => {
    if (!await keyClipboard.copy(await getKey())) throw new Error(t('extensions.mcp.copyFailed'))
  })
}
async function rotate() {
  await keyAction(async () => {
    hideKey()
    const started = keyRevision
    const key = await regenerate()
    if (disposed || started !== keyRevision) return
    fullKey.value = key
    // Keep the newly generated key visible until explicitly hidden or leaving.
    revealed.value = true
    toast.success(t('extensions.mcp.keyGenerated'))
  })
}
async function copyEndpoint() {
  if (!await endpointClipboard.copy(endpoint.value)) error.value = t('extensions.mcp.copyFailed')
}
</script>

<template>
  <Card class="min-w-0">
    <CardHeader>
      <div class="grid grid-cols-[minmax(0,1fr)_auto] items-start gap-3">
        <div class="flex min-w-0 flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
          <CardTitle>{{ t('extensions.mcp.service') }}</CardTitle>
          <span v-if="response" class="flex min-w-0 items-center gap-2 break-words text-sm" role="status">
            <span class="size-2 shrink-0 rounded-full" :class="stateClass" />
            <span class="min-w-0 break-words">{{ statusText }}</span>
          </span>
        </div>
        <div class="flex items-center gap-2">
          <Loader2 v-if="busy" class="size-4 animate-spin" />
          <Switch :model-value="response?.enabled ?? false" :disabled="busy || keyBusy || !response" :aria-label="t('extensions.mcp.title')" @update:model-value="toggle" />
        </div>
      </div>
      <p v-if="stale" class="text-xs text-muted-foreground" role="status">{{ t('extensions.mcp.stale') }}</p>
      <p v-if="error || response?.credentials_error" role="alert" class="break-words text-sm text-destructive">{{ error || response?.credentials_error }}</p>
    </CardHeader>
    <CardContent v-if="!response" class="space-y-3" :aria-label="t('extensions.mcp.loading')" aria-busy="true">
      <div class="h-5 animate-pulse rounded bg-muted" /><div class="h-5 w-2/3 animate-pulse rounded bg-muted" />
    </CardContent>
    <CardContent v-else-if="response.enabled || response.credentials_error" class="min-w-0 space-y-4">
      <div v-if="response.enabled && (!response.capabilities.absolute_mouse || !response.capabilities.keyboard || response.computer_use_active)" class="space-y-2 text-xs text-amber-600 dark:text-amber-400">
        <p v-if="!response.capabilities.absolute_mouse">{{ t('extensions.mcp.noMouse') }} <button class="underline" @click="$emit('navigate', 'hid')">{{ t('extensions.mcp.usbSettings') }}</button></p>
        <p v-if="!response.capabilities.keyboard">{{ t('extensions.mcp.noKeyboard') }} <button class="underline" @click="$emit('navigate', 'hid')">{{ t('extensions.mcp.usbSettings') }}</button></p>
        <p v-if="response.computer_use_active">{{ t('extensions.mcp.computerUseActive') }}</p>
      </div>
      <div class="grid min-w-0 gap-1 sm:grid-cols-[5rem_minmax(0,1fr)] sm:items-center sm:gap-3">
        <span class="text-sm">{{ t('extensions.mcp.address') }}</span>
        <div class="flex min-w-0 items-center gap-2">
          <span class="flex min-w-0 flex-1 font-mono text-sm" :title="endpoint" :aria-label="endpoint">
            <span class="truncate">{{ endpoint.slice(0, -12) }}</span><span class="shrink-0">{{ endpoint.slice(-12) }}</span>
          </span>
          <Button variant="ghost" size="sm" :aria-label="t(endpointClipboard.copied.value ? 'extensions.mcp.copied' : 'extensions.mcp.copyAddress')" @click="copyEndpoint">
            <Check v-if="endpointClipboard.copied.value" class="size-4" /><Copy v-else class="size-4" />
            <span class="hidden sm:inline">{{ t(endpointClipboard.copied.value ? 'extensions.mcp.copied' : 'extensions.mcp.copy') }}</span>
          </Button>
        </div>
        <span class="text-sm">{{ t('extensions.mcp.apiKey') }}</span>
        <div class="flex min-w-0 flex-wrap items-center gap-1">
          <span class="min-w-0 flex-1 break-all font-mono text-sm">{{ displayKey }}</span>
          <div class="flex shrink-0">
            <Button variant="ghost" size="sm" :disabled="busy || keyBusy" :aria-label="t(revealed ? 'extensions.mcp.hide' : 'extensions.mcp.show')" @click="reveal">
              <EyeOff v-if="revealed" class="size-4" /><Eye v-else class="size-4" /><span class="hidden sm:inline">{{ t(revealed ? 'extensions.mcp.hide' : 'extensions.mcp.show') }}</span>
            </Button>
            <Button variant="ghost" size="sm" :disabled="busy || keyBusy" :aria-label="t(keyClipboard.copied.value ? 'extensions.mcp.copied' : 'extensions.mcp.copyKey')" @click="copyKey">
              <Check v-if="keyClipboard.copied.value" class="size-4" /><Copy v-else class="size-4" /><span class="hidden sm:inline">{{ t(keyClipboard.copied.value ? 'extensions.mcp.copied' : 'extensions.mcp.copy') }}</span>
            </Button>
            <Button variant="ghost" size="sm" :disabled="busy || keyBusy" :aria-label="t('extensions.mcp.regenerate')" @click="confirming = !confirming">
              <RefreshCw class="size-4" /><span class="hidden sm:inline">{{ t('extensions.mcp.regenerate') }}</span>
            </Button>
          </div>
        </div>
        <div v-if="confirming" class="min-w-0 space-y-2 sm:col-start-2">
          <p class="text-sm">{{ t('extensions.mcp.regenerateWarning') }}</p>
          <div class="flex gap-2"><Button variant="destructive" size="sm" :disabled="busy || keyBusy" @click="rotate">{{ t('extensions.mcp.regenerate') }}</Button><Button variant="ghost" size="sm" :disabled="busy || keyBusy" @click="confirming = false">{{ t('extensions.mcp.cancel') }}</Button></div>
        </div>
        <p v-if="http" class="text-xs text-amber-600 sm:col-start-2 dark:text-amber-400">{{ t('extensions.mcp.httpWarning') }}</p>
      </div>
    </CardContent>
  </Card>
  <template v-if="response?.enabled">
    <Card class="min-w-0">
      <CardHeader><CardTitle>{{ t('extensions.mcp.connectClient') }}</CardTitle></CardHeader>
      <CardContent class="min-w-0">
        <McpClientSetup :endpoint="endpoint" :display-key="displayKey" :get-key="getKey" :disabled="busy || keyBusy" @error="error = $event" />
      </CardContent>
    </Card>
    <Card class="min-w-0">
      <CardHeader><CardTitle>{{ t('extensions.mcp.preferences') }}</CardTitle></CardHeader>
      <CardContent class="space-y-4">
        <div class="space-y-2">
          <div class="flex items-center justify-between gap-3">
            <label for="mcp-natural-scrolling" class="text-sm">{{ t('extensions.mcp.naturalScrolling') }}</label>
            <Switch id="mcp-natural-scrolling" :model-value="response.host_natural_scrolling" :disabled="busy || keyBusy" @update:model-value="toggleNaturalScrolling" />
          </div>
          <p class="text-xs text-muted-foreground">{{ t('extensions.mcp.naturalScrollingHint') }}</p>
        </div>
        <div class="space-y-2">
          <div class="flex flex-wrap items-center justify-between gap-3">
            <span class="text-sm">{{ t('extensions.mcp.screenshotSize') }}</span>
            <SegmentedChoice :model-value="String(response.screenshot_long_edge)" :label="t('extensions.mcp.screenshotSize')" :disabled="busy" :options="[1280, 1568, 1920, 2560].map(value => ({ value: String(value), label: value === 1280 ? t('extensions.mcp.recommendedSize') : String(value) }))" @update:model-value="setScreenshotSize" />
          </div>
          <p class="text-xs text-muted-foreground">{{ t('extensions.mcp.screenshotSizeHint') }}</p>
          <p v-if="sourceIsSmaller" class="text-xs text-muted-foreground">{{ t('extensions.mcp.noUpscale', { width: response.source_size?.[0], height: response.source_size?.[1] }) }}</p>
        </div>
      </CardContent>
    </Card>
    <McpActivityList :records="response.recent" :busy="busy" @clear="clearRecords" />
  </template>
</template>
