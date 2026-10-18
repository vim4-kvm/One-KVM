<script setup lang="ts">
import { computed, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Check, Copy } from 'lucide-vue-next'
import { Button } from '@/components/ui/button'
import { Tabs, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { useClipboard } from '@/composables/useClipboard'
import { codexInstallCommand } from '@/lib/mcpClientSetup'

const props = defineProps<{ endpoint: string; displayKey: string; getKey: () => Promise<string>; disabled: boolean }>()
const emit = defineEmits<{ error: [message: string] }>()
const { t } = useI18n()
const { copy, copied } = useClipboard()
const tab = ref('claude')
const copying = ref(false)
try {
  const stored = localStorage.getItem('mcp-client')
  if (stored && ['claude', 'codex', 'json'].includes(stored)) tab.value = stored
} catch { /* Optional preference. */ }
watch(tab, value => {
  try { localStorage.setItem('mcp-client', value) } catch { /* Optional preference. */ }
})
function snippet(key: string) {
  if (tab.value === 'codex') return codexInstallCommand(props.endpoint, key)
  if (tab.value === 'json') return JSON.stringify({ mcpServers: { 'one-kvm': {
    type: 'http', url: props.endpoint, headers: { Authorization: `Bearer ${key}` },
  } } }, null, 2)
  return `claude mcp add --transport http one-kvm ${props.endpoint} \\\n  --header "Authorization: Bearer ${key}"`
}
const preview = computed(() => snippet(props.displayKey))
async function copySnippet() {
  copying.value = true
  try {
    if (!await copy(snippet(await props.getKey()))) emit('error', t('extensions.mcp.copyFailed'))
  } catch (cause) {
    emit('error', cause instanceof Error ? cause.message : t('extensions.mcp.operationFailed'))
  } finally { copying.value = false }
}
</script>

<template>
  <section class="min-w-0 space-y-2">
    <Tabs v-model="tab" class="min-w-0 gap-2 sm:flex-row sm:items-center">
      <h3 class="text-sm font-medium">{{ t('extensions.mcp.connectClient') }}</h3>
      <TabsList class="max-w-full">
        <TabsTrigger value="claude">{{ t('extensions.mcp.clients.claude') }}</TabsTrigger>
        <TabsTrigger value="codex">{{ t('extensions.mcp.clients.codex') }}</TabsTrigger>
        <TabsTrigger value="json">{{ t('extensions.mcp.clients.json') }}</TabsTrigger>
      </TabsList>
    </Tabs>
    <div class="relative min-w-0 rounded-md border bg-muted/30">
      <pre class="overflow-x-auto p-3 pt-11 font-mono text-xs sm:mr-36 sm:pt-3"><code>{{ preview }}</code></pre>
      <Button class="absolute right-1 top-1 h-7 px-2 text-xs" variant="ghost" size="sm" :disabled="disabled || copying" @click="copySnippet">
        <Check v-if="copied" class="size-4" /><Copy v-else class="size-4" />
        {{ t(copied ? 'extensions.mcp.copied' : tab === 'json' ? 'extensions.mcp.copyConfig' : 'extensions.mcp.copyCommand') }}
      </Button>
    </div>
    <p v-if="tab === 'codex'" class="text-xs text-muted-foreground">{{ t('extensions.mcp.codexHint') }}</p>
  </section>
</template>
