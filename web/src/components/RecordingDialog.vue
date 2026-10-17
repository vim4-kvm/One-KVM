<script setup lang="ts">
import { onMounted, onUnmounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Circle, Download, Square, Trash2 } from 'lucide-vue-next'
import { request } from '@/api/request'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@/components/ui/dialog'

interface Status { recording: boolean; file: string | null; elapsed_seconds: number; error: string | null }
interface RecordingFile { name: string; bytes: number; modified: number; complete: boolean }
const { t } = useI18n()
const open = ref(false)
const status = ref<Status | null>(null)
const files = ref<RecordingFile[]>([])
const busy = ref(false)
const error = ref('')
let timer: ReturnType<typeof setTimeout> | undefined
let mounted = true

async function refresh() {
  try {
    status.value = await request<Status>('/recording', {}, { toastOnError: false })
    if (open.value) files.value = await request<RecordingFile[]>('/recording/files', {}, { toastOnError: false })
    error.value = ''
  } catch (e) { error.value = e instanceof Error ? e.message : String(e) }
}
async function poll() {
  if (!busy.value) await refresh()
  if (mounted) timer = setTimeout(poll, 2000)
}
async function change(action: 'start' | 'stop') {
  busy.value = true
  error.value = ''
  try {
    status.value = await request<Status>(`/recording/${action}`, { method: 'POST' })
    await refresh()
  } catch (e) { error.value = e instanceof Error ? e.message : String(e) }
  finally { busy.value = false }
}
async function remove(file: RecordingFile) {
  if (!window.confirm(t('recording.confirmDelete'))) return
  busy.value = true
  try {
    await request(`/recording/files/${encodeURIComponent(file.name)}`, { method: 'DELETE' })
    await refresh()
  } catch (e) { error.value = e instanceof Error ? e.message : String(e) }
  finally { busy.value = false }
}
function active(file: RecordingFile) { return status.value?.recording && status.value.file === file.name }
function elapsed(seconds: number) {
  return `${Math.floor(seconds / 3600).toString().padStart(2, '0')}:${Math.floor(seconds / 60 % 60).toString().padStart(2, '0')}:${(seconds % 60).toString().padStart(2, '0')}`
}
watch(open, value => { if (value) void refresh() })
onMounted(() => { void poll() })
onUnmounted(() => { mounted = false; clearTimeout(timer) })
</script>

<template>
  <Button variant="ghost" size="sm" class="gap-1.5 rounded-lg text-xs" :title="t('recording.title')" :aria-label="t('recording.title')" @click="open = true">
    <Circle class="size-4" :class="status?.recording ? 'fill-red-500 text-red-500 animate-pulse' : ''" />
    <span v-if="status?.recording">{{ elapsed(status.elapsed_seconds) }}</span>
    <span v-else class="hidden lg:inline">{{ t('recording.title') }}</span>
  </Button>
  <Dialog v-model:open="open">
    <DialogContent class="sm:max-w-xl">
      <DialogHeader>
        <DialogTitle>{{ t('recording.title') }}</DialogTitle>
        <DialogDescription>{{ t('recording.hint') }}</DialogDescription>
      </DialogHeader>
      <p class="text-xs text-muted-foreground">{{ t('recording.requirements') }}</p>
      <p v-if="error || status?.error" role="alert" class="text-sm text-destructive break-words">{{ error || status?.error }}</p>
      <div class="flex items-center gap-3">
        <Button v-if="status?.recording" :disabled="busy" variant="destructive" @click="change('stop')">
          <Square class="size-4" />{{ t('recording.stop') }} · {{ elapsed(status.elapsed_seconds) }}
        </Button>
        <Button v-else :disabled="busy || !status || !!error" @click="change('start')"><Circle class="size-4" />{{ t('recording.start') }}</Button>
        <span v-if="busy" role="status" class="text-xs text-muted-foreground">{{ t('recording.processing') }}</span>
      </div>
      <div class="max-h-80 overflow-auto space-y-2">
        <p v-if="!files.length" class="text-sm text-muted-foreground">{{ t('recording.empty') }}</p>
        <div v-for="file in files" :key="file.name" class="flex items-center gap-2 rounded border p-2">
          <div class="min-w-0 flex-1 text-xs">
            <p>{{ new Date(file.modified * 1000).toLocaleString() }} · {{ (file.bytes / 1048576).toFixed(1) }} MiB</p>
            <p class="truncate text-muted-foreground" :title="file.name">{{ file.name }}</p>
            <p v-if="active(file)" class="text-red-500">{{ t('recording.active') }}</p>
            <p v-else-if="!file.complete" class="text-amber-600">{{ t('recording.partial') }}</p>
          </div>
          <Button v-if="!active(file)" as-child variant="ghost" size="icon-sm">
            <a :href="`/api/recording/files/${encodeURIComponent(file.name)}`" :aria-label="t('recording.download')" :title="t('recording.download')" download><Download class="size-4" /></a>
          </Button>
          <Button variant="ghost" size="icon-sm" :disabled="busy || active(file)" :aria-label="t('recording.delete')" :title="t('recording.delete')" @click="remove(file)"><Trash2 class="size-4" /></Button>
        </div>
      </div>
    </DialogContent>
  </Dialog>
</template>
