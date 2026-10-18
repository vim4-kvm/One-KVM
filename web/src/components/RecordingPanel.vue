<script setup lang="ts">
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { AlertTriangle, CircleAlert, Download, Film, Square, Trash2, X } from 'lucide-vue-next'
import { Button } from '@/components/ui/button'
import { Badge } from '@/components/ui/badge'
import { Spinner } from '@/components/ui/spinner'
import RecordingLocationPicker from '@/components/RecordingLocationPicker.vue'
import {
  AlertDialog, AlertDialogCancel, AlertDialogContent, AlertDialogDescription,
  AlertDialogFooter, AlertDialogHeader, AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import {
  formatBytes, formatClock, localDownloadName, reasonText, recordingDownloadUrl, useDeviceRecording, type RecordingFile,
} from '@/composables/useDeviceRecording'

defineProps<{ videoMode?: string; inlineLocationPicker?: boolean }>()
const emit = defineEmits<{ (e: 'switchCodec', codec: 'h264'): void }>()
const { t, locale } = useI18n()
const recording = useDeviceRecording()
const { status, files, unavailableLocations, unreachable, action, elapsedSeconds, bitrateBps, remainingSeconds, stoppedLocation, dismissedStop } = recording
function locationName(location: { id: string; name: string }) { return location.id === 'local' ? t('recording.local') : location.name }

const pendingDelete = ref<RecordingFile | null>(null)
const fileLocationFilter = ref('all')

const isRecording = computed(() => !!status.value?.recording)
const blocker = computed(() => isRecording.value ? null : status.value?.storage_reason ? 'storage_unavailable' : status.value?.blocker ?? null)
const canStart = computed(() => !!status.value && !isRecording.value && !blocker.value && !action.value && !unreachable.value)
const lowRemaining = computed(() => remainingSeconds.value !== null && remainingSeconds.value < 600)
const freeBytes = computed(() => {
  const storage = status.value?.storage
  return storage ? Math.max(0, storage.free_bytes - storage.reserve_bytes) : null
})
const lastError = computed(() => {
  const current = status.value
  if (!current || current.recording || !(current.error_code || current.error)) return null
  const key = current.started_at !== null ? String(current.started_at) : `start:${current.error_code ?? current.error}`
  if (dismissedStop.value === key) return null
  const interrupted = !!current.file
  const location = files.value.find(file => file.name === current.file)?.location ?? stoppedLocation.value ?? current.location
  const name = locationName(location)
  const time = current.started_at === null ? null : new Date((current.started_at + current.elapsed_seconds) * 1000)
    .toLocaleTimeString(locale.value, { hour: '2-digit', minute: '2-digit', hour12: false })
  const title = !interrupted ? t('recording.startFailedTitle') : time ? t('recording.stoppedAt', { time }) : t('recording.interruptedTitle')
  const params = { name, duration: formatClock(current.elapsed_seconds) }
  const reason = interrupted && current.error_code === 'storage_unavailable' ? t('recording.storageStopped', params)
    : interrupted && current.error_code === 'low_storage' ? t('recording.spaceStopped', params)
    : reasonText(current.error_code, current.error)
  const recovered = interrupted && ['storage_unavailable', 'low_storage'].includes(current.error_code ?? '')
    && current.location.id === location.id && current.location.folder === location.folder
    && !!current.storage && !current.storage_reason && !current.blocker && !unreachable.value
  return { key, title, reason, recovered: recovered ? t('recording.storageRecovered', { name }) : null }
})
const totalBytes = computed(() => files.value.reduce((sum, file) => sum + file.bytes, 0))
const fileLocations = computed(() => [...new Map(files.value.map(file => [file.location.id, file.location])).values()])
const filteredFiles = computed(() => fileLocationFilter.value === 'all' ? files.value : files.value.filter(file => file.location.id === fileLocationFilter.value))
watch(fileLocations, locations => {
  if (locations.length < 2 || !locations.some(location => location.id === fileLocationFilter.value)) fileLocationFilter.value = 'all'
})

onMounted(() => recording.setPanelOpen(true))
onUnmounted(() => recording.setPanelOpen(false))

function isActive(file: RecordingFile) {
  return isRecording.value && status.value?.file === file.name && status.value.location.id === file.location.id
}
function codecLabel(codec: string | null | undefined) {
  return codec === 'h265' ? 'H.265' : codec === 'h264' ? 'H.264' : null
}
function fileTitle(file: RecordingFile) {
  const date = new Date((file.started_at ?? file.modified) * 1000)
  const time = date.toLocaleTimeString(locale.value, { hour: '2-digit', minute: '2-digit' })
  const today = new Date()
  if (date.toDateString() === today.toDateString()) return t('recording.today', { time })
  return date.toLocaleString(locale.value, { month: 'short', day: 'numeric', hour: '2-digit', minute: '2-digit' })
}
function fileMeta(file: RecordingFile) {
  return [
    file.duration_seconds !== null ? formatClock(file.duration_seconds) : null,
    codecLabel(file.codec),
    formatBytes(file.bytes),
  ].filter(Boolean).join(' · ')
}
function remainingText(seconds: number) {
  if (seconds >= 3600) return t('recording.remainingHours', { hours: Math.floor(seconds / 3600) })
  return t('recording.remainingMinutes', { minutes: Math.max(1, Math.floor(seconds / 60)) })
}
function confirmDelete() {
  const file = pendingDelete.value
  pendingDelete.value = null
  if (file) void recording.remove(file)
}
</script>

<template>
  <div class="flex min-w-0 flex-col gap-4 p-4" data-recording-panel>
    <header>
      <h2 class="text-sm font-semibold">{{ t('recording.title') }}</h2>
      <p class="mt-0.5 text-xs text-muted-foreground">{{ t('recording.hint') }}</p>
    </header>

    <section
      :class="[
        'rounded-xl border p-4 transition-colors',
        isRecording ? 'border-red-500/30 bg-red-500/[0.06]' : 'bg-muted/30',
      ]"
      :aria-busy="!!action"
    >
      <RecordingLocationPicker :inline="inlineLocationPicker" />
      <div v-if="isRecording" class="flex flex-wrap items-center gap-x-4 gap-y-3">
        <div class="min-w-0 basis-48 flex-1">
          <div class="flex items-center gap-2 text-xs font-semibold tracking-wider text-red-600 dark:text-red-400">
            <span class="relative flex size-2">
              <span class="absolute inline-flex size-full animate-ping rounded-full bg-red-500/60" />
              <span class="relative inline-flex size-2 rounded-full bg-red-500" />
            </span>
            {{ t('recording.active') }}
          </div>
          <p class="mt-1 font-mono text-3xl font-semibold tabular-nums leading-none" role="timer">
            {{ formatClock(elapsedSeconds, true) }}
          </p>
          <p class="mt-2 text-xs text-muted-foreground">
            {{ [codecLabel(status?.codec), t('recording.withAudio'), status?.bytes ? formatBytes(status.bytes) : null,
                bitrateBps ? `${(bitrateBps / 1e6).toFixed(1)} Mbps` : null].filter(Boolean).join(' · ') }}
          </p>
          <p v-if="remainingSeconds !== null" :class="['mt-1 whitespace-nowrap text-xs', lowRemaining ? 'font-medium text-amber-600 dark:text-amber-400' : 'text-muted-foreground']">
            {{ remainingText(remainingSeconds) }}
          </p>
        </div>
        <Button variant="destructive" class="w-full sm:w-auto sm:min-w-32" :disabled="!!action" @click="recording.stop()">
          <Spinner v-if="action === 'stopping'" />
          <Square v-else class="size-3.5 fill-current" />
          {{ action === 'stopping' ? t('recording.saving') : t('recording.stop') }}
        </Button>
      </div>

      <div v-else class="flex flex-wrap items-center gap-x-4 gap-y-3">
        <div class="min-w-0 basis-48 flex-1">
          <template v-if="unreachable">
            <p class="flex items-center gap-1.5 text-sm font-medium"><CircleAlert class="size-4 text-destructive" />{{ t('recording.unreachable') }}</p>
            <p class="mt-1 text-xs text-muted-foreground">{{ t('recording.unreachableHint') }}</p>
          </template>
          <template v-else-if="blocker">
            <p class="flex items-center gap-1.5 text-sm font-medium"><AlertTriangle class="size-4 text-amber-500" />{{ t('recording.blocked') }}</p>
            <p class="mt-1 text-xs text-muted-foreground">{{ t(`recording.blockers.${blocker}`, t('recording.reasons.internal')) }}</p>
            <p v-if="status?.storage_reason" class="mt-1 text-xs text-muted-foreground">{{ t(`recording.locationReasons.${status.storage_reason}`) }}</p>
            <Button v-if="blocker === 'video_codec'" variant="outline" size="sm" class="mt-2.5" @click="emit('switchCodec', 'h264')">
              {{ t('recording.switchToH264') }}
            </Button>
          </template>
          <template v-else-if="action === 'starting'">
            <p class="text-sm font-medium">{{ t('recording.starting') }}</p>
            <p class="mt-1 text-xs text-muted-foreground">{{ t('recording.startingHint') }}</p>
          </template>
          <template v-else>
            <p class="text-sm font-medium">{{ status ? t('recording.ready') : t('recording.loading') }}</p>
            <p v-if="status" class="mt-1 text-xs text-muted-foreground">
              {{ [codecLabel(videoMode), t('recording.withAudio')].filter(Boolean).join(' · ') }}
            </p>
            <p v-if="status && freeBytes !== null" class="mt-1 flex flex-wrap items-baseline gap-x-1 text-xs text-muted-foreground">
              <span class="max-w-full truncate" :title="locationName(status.location)">{{ locationName(status.location) }}</span>
              <span class="whitespace-nowrap">{{ t('recording.free', { size: formatBytes(freeBytes) }) }}</span>
              <span v-if="remainingSeconds !== null" class="whitespace-nowrap">· {{ remainingText(remainingSeconds) }}</span>
            </p>
          </template>
        </div>
        <Button class="w-full bg-red-600 sm:w-auto sm:min-w-32 text-white hover:bg-red-600/90" :disabled="!canStart" @click="recording.start()">
          <Spinner v-if="action === 'starting'" />
          <span v-else class="inline-flex size-3 rounded-full bg-white" />
          {{ t('recording.start') }}
        </Button>
      </div>
    </section>

    <div
      v-if="lastError"
      role="alert"
      class="-mt-2 flex items-start gap-2.5 rounded-lg border border-amber-500/30 bg-amber-500/10 px-3 py-2.5 text-sm text-amber-800 dark:text-amber-300"
    >
      <AlertTriangle class="mt-0.5 size-4 shrink-0" />
      <div class="min-w-0 flex-1">
        <p class="font-medium">{{ lastError.title }}</p>
        <p class="break-words text-xs opacity-90">{{ lastError.reason }} <span v-if="lastError.recovered" class="text-green-700 dark:text-green-400">{{ lastError.recovered }}</span></p>
      </div>
      <button type="button" class="rounded p-0.5 opacity-70 hover:opacity-100" :aria-label="t('recording.dismiss')" @click="recording.dismissStop(lastError.key)">
        <X class="size-4" />
      </button>
    </div>

    <section class="space-y-2">
      <p v-for="location in unavailableLocations.filter(l => l.id !== 'local' && l.reason === 'unresponsive')" :key="location.id" class="break-words text-xs text-amber-600 dark:text-amber-400">{{ t('recording.nasOmitted', { name: location.name }) }}</p>
      <div class="flex items-baseline justify-between">
        <h3 class="text-sm font-medium">
          {{ t('recording.library') }}
          <span v-if="files.length" class="ml-1 font-normal text-muted-foreground">{{ files.length }}</span>
        </h3>
        <span v-if="files.length" class="whitespace-nowrap text-xs text-muted-foreground">{{ t('recording.totalSize', { size: formatBytes(totalBytes) }) }}</span>
      </div>

      <div v-if="fileLocations.length >= 2" role="group" :aria-label="t('recording.filterLocation')" class="flex flex-wrap gap-1 rounded-lg bg-muted/50 p-1">
        <Button size="sm" :variant="fileLocationFilter === 'all' ? 'secondary' : 'ghost'" :aria-pressed="fileLocationFilter === 'all'" @click="fileLocationFilter = 'all'">{{ t('recording.allLocations') }}</Button>
        <Button v-for="location in fileLocations" :key="location.id" size="sm" class="max-w-full min-w-0"
          :variant="fileLocationFilter === location.id ? 'secondary' : 'ghost'" :aria-pressed="fileLocationFilter === location.id"
          @click="fileLocationFilter = location.id"><span class="truncate" :title="locationName(location)">{{ locationName(location) }}</span></Button>
      </div>

      <div v-if="!files.length" class="flex flex-col items-center rounded-xl border border-dashed px-4 py-8 text-center">
        <div class="mb-2 flex size-10 items-center justify-center rounded-lg bg-muted"><Film class="size-5 text-muted-foreground" /></div>
        <p class="text-sm font-medium">{{ t('recording.empty') }}</p>
        <p class="mt-1 text-xs text-muted-foreground">{{ t('recording.emptyHint') }}</p>
      </div>

      <ul v-else class="-mx-1 max-h-72 space-y-1 overflow-y-auto px-1">
        <li
          v-for="file in filteredFiles"
          :key="`${file.location.id}:${file.name}`"
          :data-recording="file.name"
          :class="['group flex items-center gap-3 rounded-lg border px-3 py-2.5 transition-colors', isActive(file) ? 'border-red-500/30 bg-red-500/[0.04]' : 'hover:bg-muted/50']"
        >
          <div :class="['flex size-9 shrink-0 items-center justify-center rounded-md', isActive(file) ? 'bg-red-500/10 text-red-500' : 'bg-muted text-muted-foreground']">
            <Film class="size-4" />
          </div>
          <div class="min-w-0 flex-1">
            <div class="flex items-center gap-2">
              <p class="truncate text-sm font-medium">{{ fileTitle(file) }}</p>
              <Badge v-if="isActive(file)" variant="destructive" class="px-1.5 py-0 text-[10px]">{{ t('recording.activeBadge') }}</Badge>
              <Badge v-else-if="!file.complete" variant="warning" class="px-1.5 py-0 text-[10px]">{{ t('recording.partialBadge') }}</Badge>
            </div>
            <p class="truncate text-xs text-muted-foreground tabular-nums">{{ isActive(file) ? t('recording.inProgress') : fileMeta(file) }}</p>
            <Badge v-if="fileLocations.length >= 2" variant="secondary" class="max-w-full truncate px-1.5 py-0 text-[10px]">{{ locationName(file.location) }}</Badge>
            <p v-if="!file.complete && !isActive(file)" class="truncate text-xs text-muted-foreground" :title="t('recording.partial')">{{ t('recording.partial') }}</p>
            <p v-else-if="file.stop_reason && !isActive(file)" class="truncate text-xs text-muted-foreground" :title="reasonText(file.stop_reason)">
              {{ t('recording.stoppedBecause', { reason: reasonText(file.stop_reason) }) }}
            </p>
          </div>
          <div v-if="!isActive(file)" class="flex shrink-0 items-center gap-0.5">
            <Button as-child variant="ghost" size="icon-sm">
              <a :href="recordingDownloadUrl(file)" :download="localDownloadName(file)" :aria-label="t('recording.download')" :title="t('recording.download')"><Download class="size-4" /></a>
            </Button>
            <Button
              variant="ghost"
              size="icon-sm"
              class="text-muted-foreground hover:text-destructive"
              :disabled="!!action"
              :aria-label="t('recording.delete')"
              :title="t('recording.delete')"
              @click="pendingDelete = file"
            >
              <Trash2 class="size-4" />
            </Button>
          </div>
        </li>
      </ul>
    </section>

    <p class="border-t pt-3 text-xs leading-relaxed text-muted-foreground">{{ t('recording.footer') }}</p>

    <AlertDialog :open="!!pendingDelete" @update:open="value => { if (!value) pendingDelete = null }">
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{{ t('recording.confirmDeleteTitle') }}</AlertDialogTitle>
          <AlertDialogDescription>
            {{ pendingDelete ? t('recording.confirmDelete', { name: `${fileTitle(pendingDelete)} · ${fileMeta(pendingDelete)}` }) : '' }}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel>{{ t('common.cancel') }}</AlertDialogCancel>
          <Button variant="destructive" @click="confirmDelete">{{ t('recording.delete') }}</Button>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  </div>
</template>
