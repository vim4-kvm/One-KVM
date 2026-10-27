<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { request } from '@/api/request'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Switch } from '@/components/ui/switch'
import { Badge } from '@/components/ui/badge'

interface OutputStatus { state: string; error: string | null; codec: string | null; frames: number; bytes_sent: number; clients: number; restarts: number }
interface Settings {
  srt: { enabled: boolean; port: number; latency_ms: number; audio: boolean; passphrase_set: boolean }
  rtmp: { enabled: boolean; audio: boolean; url_set: boolean }
}
interface Response { config: Settings; status: { srt: OutputStatus; rtmp: OutputStatus } }
const { t } = useI18n()
const data = ref<Response | null>(null)
const form = ref<Settings | null>(null)
const passphrase = ref('')
const clearPassphrase = ref(false)
const url = ref('')
const busy = ref(false)
const error = ref('')
let disposed = false
let timer: ReturnType<typeof setTimeout> | undefined
const srtUrl = computed(() => {
  const host = window.location.hostname
  return `srt://${host.includes(':') ? `[${host}]` : host}:${data.value?.config.srt.port ?? 8890}?mode=caller`
})
async function refresh() {
  try {
    const response = await request<Response>('/broadcast', {}, { toastOnError: false })
    if (disposed) return
    data.value = response
    if (!form.value) form.value = structuredClone(response.config)
    error.value = ''
  } catch {
    if (!disposed) error.value = t('broadcast.unavailable')
  } finally {
    if (!disposed) timer = setTimeout(refresh, 2000)
  }
}
async function save() {
  if (!form.value) return
  busy.value = true
  error.value = ''
  try {
    await request('/broadcast', { method: 'PATCH', body: JSON.stringify({
      srt: { enabled: form.value.srt.enabled, port: Number(form.value.srt.port), latency_ms: Number(form.value.srt.latency_ms), audio: form.value.srt.audio,
        ...(clearPassphrase.value ? { passphrase: '' } : passphrase.value ? { passphrase: passphrase.value } : {}) },
      rtmp: { enabled: form.value.rtmp.enabled, audio: form.value.rtmp.audio, ...(url.value ? { url: url.value.trim() } : {}) },
    }) })
    passphrase.value = ''; url.value = ''; clearPassphrase.value = false
    const response = await request<Response>('/broadcast')
    if (!disposed) { data.value = response; form.value = structuredClone(response.config) }
  } catch (e) {
    error.value = e instanceof Error ? e.message : t('broadcast.unavailable')
  } finally { busy.value = false }
}
function stateText(status?: OutputStatus) {
  return status?.state ? t(`broadcast.states.${status.state}`) : t('common.loading')
}
onMounted(refresh)
onBeforeUnmount(() => { disposed = true; clearTimeout(timer) })
</script>

<template>
  <Card>
    <CardHeader>
      <CardTitle>{{ t('broadcast.title') }}</CardTitle>
      <CardDescription>{{ t('broadcast.hint') }}</CardDescription>
    </CardHeader>
    <CardContent class="space-y-6">
      <p v-if="error" role="alert" class="text-sm text-destructive">{{ error }}</p>
      <template v-if="form">
        <fieldset :disabled="busy" class="space-y-6">
          <section class="space-y-3">
            <div class="flex items-center justify-between gap-3">
              <Label for="broadcast-srt">SRT</Label>
              <div class="flex items-center gap-3"><Badge variant="secondary">{{ stateText(data?.status.srt) }}</Badge><Switch id="broadcast-srt" v-model="form.srt.enabled" /></div>
            </div>
            <p class="text-sm text-muted-foreground">{{ t('broadcast.srtHint') }}</p>
            <div class="grid gap-4 sm:grid-cols-2">
              <div class="space-y-2"><Label for="srt-port">{{ t('broadcast.port') }}</Label><Input id="srt-port" v-model.number="form.srt.port" type="number" min="1024" max="65535" /></div>
              <div class="space-y-2"><Label for="srt-latency">{{ t('broadcast.latency') }}</Label><Input id="srt-latency" v-model.number="form.srt.latency_ms" type="number" min="20" max="8000" /></div>
            </div>
            <div class="space-y-2"><Label for="srt-passphrase">{{ t('broadcast.passphrase') }}</Label><Input id="srt-passphrase" v-model="passphrase" type="password" autocomplete="new-password" :disabled="clearPassphrase" :placeholder="form.srt.passphrase_set ? t('broadcast.keepSecret') : t('broadcast.optionalSecret')" /></div>
            <label v-if="form.srt.passphrase_set" class="flex items-center gap-2 text-sm"><input v-model="clearPassphrase" type="checkbox" />{{ t('broadcast.clearPassphrase') }}</label>
            <div class="flex items-center justify-between"><Label for="srt-audio">{{ t('broadcast.audio') }}</Label><Switch id="srt-audio" v-model="form.srt.audio" /></div>
            <div class="space-y-1"><Label>{{ t('broadcast.receiveUrl') }}</Label><code class="block break-all rounded bg-muted p-2 text-xs select-all">{{ srtUrl }}</code><p v-if="data?.config.srt.passphrase_set" class="text-xs text-muted-foreground">{{ t('broadcast.receiverSecret') }}</p></div>
            <p v-if="data?.status.srt.state === 'streaming'" class="text-xs text-muted-foreground">{{ data.status.srt.codec?.toUpperCase() }} · {{ data.status.srt.clients }} {{ t('broadcast.clients') }} · {{ (data.status.srt.bytes_sent / 1048576).toFixed(1) }} MiB</p>
            <p v-if="data?.status.srt.error" class="text-sm text-muted-foreground">{{ data.status.srt.error }}</p>
          </section>
          <section class="space-y-3 border-t pt-4">
            <div class="flex items-center justify-between gap-3"><Label for="broadcast-rtmp">RTMP / RTMPS</Label><div class="flex items-center gap-3"><Badge variant="secondary">{{ stateText(data?.status.rtmp) }}</Badge><Switch id="broadcast-rtmp" v-model="form.rtmp.enabled" /></div></div>
            <p class="text-sm text-muted-foreground">{{ t('broadcast.rtmpHint') }}</p>
            <div class="space-y-2"><Label for="rtmp-url">{{ t('broadcast.publishUrl') }}</Label><Input id="rtmp-url" v-model="url" type="password" autocomplete="new-password" :placeholder="form.rtmp.url_set ? t('broadcast.keepUrl') : 'rtmp://server/live/stream-key'" /></div>
            <div class="flex items-center justify-between"><Label for="rtmp-audio">{{ t('broadcast.audio') }}</Label><Switch id="rtmp-audio" v-model="form.rtmp.audio" /></div>
            <p v-if="data?.status.rtmp.state === 'streaming'" class="text-xs text-muted-foreground">H.264 · {{ (data.status.rtmp.bytes_sent / 1048576).toFixed(1) }} MiB</p>
            <p v-if="data?.status.rtmp.error" class="text-sm text-muted-foreground">{{ data.status.rtmp.error }}</p>
          </section>
          <Button :disabled="busy" @click="save">{{ t('broadcast.save') }}</Button>
        </fieldset>
      </template>
    </CardContent>
  </Card>
</template>
