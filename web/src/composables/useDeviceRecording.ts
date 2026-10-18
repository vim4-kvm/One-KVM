import { computed, ref } from 'vue'
import { toast } from 'vue-sonner'
import i18n from '@/i18n'
import { request } from '@/api/request'

export type RecordingCode =
  | 'already_recording' | 'video_codec' | 'audio_unavailable' | 'low_storage' | 'storage_error'
  | 'video_unavailable' | 'start_timeout' | 'codec_changed' | 'resolution_changed' | 'packets_lost'
  | 'capture_timeout' | 'pipeline_stopped' | 'save_failed' | 'internal' | 'storage_unavailable'

export interface RecordingLocation { id: string; name: string; folder: string }
export interface LocationAvailability extends RecordingLocation {
  available: boolean
  reason?: 'not_mounted' | 'unresponsive' | 'not_writable'
  free_bytes?: number
}

export interface RecordingStatus {
  location: RecordingLocation
  storage_reason: LocationAvailability['reason'] | null
  recording: boolean
  file: string | null
  elapsed_seconds: number
  error: string | null
  error_code: RecordingCode | null
  started_at: number | null
  codec: 'h264' | 'h265' | null
  bytes: number
  storage: { free_bytes: number; reserve_bytes: number } | null
  blocker: RecordingCode | null
}

export interface RecordingFile {
  location: RecordingLocation
  name: string
  bytes: number
  modified: number
  complete: boolean
  started_at: number | null
  duration_seconds: number | null
  codec: 'h264' | 'h265' | null
  stop_reason: RecordingCode | null
  download_name: string
}

const IDLE_POLL_MS = 10000
const ACTIVE_POLL_MS = 2000

const status = ref<RecordingStatus | null>(null)
const files = ref<RecordingFile[]>([])
const locations = ref<LocationAvailability[]>([])
const unavailableLocations = ref<LocationAvailability[]>([])
const unreachable = ref(false)
const action = ref<'starting' | 'stopping' | 'deleting' | 'location' | null>(null)
const panelOpen = ref(false)
const fetchedAt = ref(0)
const now = ref(Date.now())
const openRequests = ref(0)
const codecSwitchPrompt = ref<((confirmed: boolean) => void) | null>(null)
const stoppedLocation = ref<RecordingLocation | null>(null)
const dismissalKey = 'recording.dismissedStop'
const dismissedStop = ref<string | null>(readDismissedStop())

function readDismissedStop() {
  try { return localStorage.getItem(dismissalKey) } catch { return null }
}

function dismissStop(key: string | null) {
  if (dismissedStop.value === key) return
  dismissedStop.value = key
  try {
    if (key === null) localStorage.removeItem(dismissalKey)
    else localStorage.setItem(dismissalKey, key)
  } catch {
    // Keep dismissal working for this page when browser storage is unavailable.
  }
}

let users = 0
let pollTimer: ReturnType<typeof setTimeout> | undefined
let clockTimer: ReturnType<typeof setInterval> | undefined
let stopRequested = false
let locationRevision = 0

function t(key: string, params?: Record<string, unknown>) {
  return String(i18n.global.t(key, params as never))
}

export function localDownloadName(file: RecordingFile) {
  const date = new Date((file.started_at ?? file.modified) * 1000)
  const pad = (value: number) => value.toString().padStart(2, '0')
  return `one-kvm-${date.getFullYear()}${pad(date.getMonth() + 1)}${pad(date.getDate())}-${pad(date.getHours())}${pad(date.getMinutes())}${pad(date.getSeconds())}.mkv`
}

export function recordingDownloadUrl(file: RecordingFile) {
  return `/api/recording/files/${encodeURIComponent(file.name)}?filename=${localDownloadName(file)}&${fileLocationQuery(file)}`
}

function fileLocationQuery(file: RecordingFile) {
  return `location=${encodeURIComponent(file.location.id)}&folder=${encodeURIComponent(file.location.folder)}`
}

export function downloadRecording(file: RecordingFile) {
  const link = document.createElement('a')
  link.href = recordingDownloadUrl(file)
  link.download = localDownloadName(file)
  link.click()
}

export function formatBytes(bytes: number) {
  if (bytes >= 1024 ** 3) return `${(bytes / 1024 ** 3).toFixed(1)} GB`
  if (bytes >= 1024 ** 2) return `${(bytes / 1024 ** 2).toFixed(1)} MB`
  return `${Math.max(1, Math.round(bytes / 1024))} KB`
}

export function formatClock(seconds: number, alwaysHours = false) {
  const s = Math.max(0, Math.floor(seconds))
  const hh = Math.floor(s / 3600)
  const mm = Math.floor(s / 60) % 60
  const ss = (s % 60).toString().padStart(2, '0')
  if (hh || alwaysHours) return `${hh.toString().padStart(2, '0')}:${mm.toString().padStart(2, '0')}:${ss}`
  return `${mm}:${ss}`
}

export function reasonText(code: RecordingCode | null | undefined, fallback?: string | null) {
  if (code && i18n.global.te(`recording.reasons.${code}`)) return t(`recording.reasons.${code}`)
  return fallback || t('recording.reasons.internal')
}

async function fetchFiles() {
  const result = await request<{ files: RecordingFile[]; unavailable: LocationAvailability[] }>('/recording/files?details=true', {}, { toastOnError: false })
  files.value = result.files
  unavailableLocations.value = result.unavailable
}

async function refreshLocations() {
  locations.value = await request<LocationAvailability[]>('/recording/locations', {}, { toastOnError: false })
}

function announceTransition(previous: RecordingStatus | null, next: RecordingStatus) {
  if (!previous?.recording || next.recording) return
  const byUser = stopRequested
  stopRequested = false
  const saved = files.value.find(file => file.name === next.file && file.location.id === next.location.id
    && file.location.folder === next.location.folder && file.complete)
  if (next.error_code || next.error) {
    toast.error(t('recording.toast.interrupted'), {
      description: reasonText(next.error_code, next.error),
      action: saved ? { label: t('recording.download'), onClick: () => downloadRecording(saved) } : undefined,
    })
  } else if (byUser || saved) {
    toast.success(t('recording.toast.saved'), {
      description: saved ? `${formatClock(saved.duration_seconds ?? next.elapsed_seconds)} · ${formatBytes(saved.bytes)}` : undefined,
      action: saved ? { label: t('recording.download'), onClick: () => downloadRecording(saved) } : undefined,
    })
  }
}

async function refresh() {
  try {
    const previous = status.value
    const revision = locationRevision
    const next = await request<RecordingStatus>('/recording', {}, { toastOnError: false })
    if (revision !== locationRevision) return
    const finished = previous?.recording && !next.recording
    if (next.recording) {
      dismissStop(null)
      stoppedLocation.value = null
    } else if (next.file && (next.error_code || next.error) && (!stoppedLocation.value || previous?.started_at !== next.started_at)) {
      // The status API reports the selected destination, even for an older stopped recording.
      stoppedLocation.value = previous?.recording ? previous.location : next.location
    }
    status.value = next
    fetchedAt.value = Date.now()
    if (panelOpen.value || finished || !files.value.length) await fetchFiles()
    unreachable.value = false
    announceTransition(previous, next)
  } catch {
    unreachable.value = true
  }
}

function schedule() {
  clearTimeout(pollTimer)
  if (!users) return
  const delay = status.value?.recording || panelOpen.value ? ACTIVE_POLL_MS : IDLE_POLL_MS
  pollTimer = setTimeout(async () => {
    if (!action.value) await refresh()
    schedule()
  }, delay)
}

async function run(kind: NonNullable<typeof action.value>, task: () => Promise<unknown>) {
  action.value = kind
  try {
    await task()
  } catch {
    // Start failures are reported through status.error_code; other requests toast their own errors.
  } finally {
    await refresh()
    action.value = null
    schedule()
  }
}

const start = () => run('starting', () => request('/recording/start', { method: 'POST' }, { toastOnError: false }))
const stop = () => run('stopping', () => {
  stopRequested = true
  return request('/recording/stop', { method: 'POST' })
})
const remove = (file: RecordingFile) => run('deleting', async () => {
  await request(`/recording/files/${encodeURIComponent(file.name)}?${fileLocationQuery(file)}`, { method: 'DELETE' })
  toast.success(t('recording.toast.deleted'))
})

const elapsedSeconds = computed(() => {
  const current = status.value
  if (!current?.recording) return current?.elapsed_seconds ?? 0
  return current.elapsed_seconds + Math.max(0, now.value - fetchedAt.value) / 1000
})

const bitrateBps = computed(() => {
  const current = status.value
  if (!current?.recording || current.elapsed_seconds < 5 || !current.bytes) return null
  return (current.bytes * 8) / current.elapsed_seconds
})

const remainingSeconds = computed(() => {
  const storage = status.value?.storage
  if (!storage) return null
  let bitrate = bitrateBps.value
  if (!status.value?.recording) {
    // The library is ordered newest first across all locations.
    const latest = files.value.find(file => file.complete && (file.duration_seconds ?? 0) >= 10 && file.bytes > 0)
    bitrate = latest ? latest.bytes * 8 / latest.duration_seconds! : null
  }
  if (!bitrate) return null
  return Math.max(0, storage.free_bytes - storage.reserve_bytes) * 8 / bitrate
})

export function useDeviceRecording() {
  return {
    status,
    files,
    locations,
    unavailableLocations,
    stoppedLocation,
    dismissedStop,
    dismissStop,
    refreshLocations,
    async setLocation(location: string, folder: string) {
      if (action.value || status.value?.recording) throw new Error('Recording is busy')
      action.value = 'location'
      locationRevision++
      try {
        await request('/recording/location', { method: 'PUT', body: JSON.stringify({ location, folder }) }, { toastOnError: false })
        const selected = locations.value.find(entry => entry.id === location)
        if (selected) {
          if (location !== 'local') selected.folder = folder
          if (status.value) status.value = { ...status.value, location: { id: selected.id, name: selected.name, folder: selected.folder } }
        }
      } finally {
        await refresh()
        action.value = null
        schedule()
      }
    },
    unreachable,
    action,
    panelOpen,
    elapsedSeconds,
    bitrateBps,
    remainingSeconds,
    codecSwitchPrompt,
    openRequests,
    requestOpen() { openRequests.value++ },
    attach() {
      if (users++ === 0) {
        void refresh().then(schedule)
        clockTimer = setInterval(() => { now.value = Date.now() }, 1000)
      }
    },
    detach() {
      if (--users > 0) return
      clearTimeout(pollTimer)
      clearInterval(clockTimer)
    },
    setPanelOpen(open: boolean) {
      panelOpen.value = open
      if (open) void refresh()
      schedule()
    },
    start,
    stop,
    remove,
    /** Resolves true when no recording would be interrupted, or the user accepts stopping it. */
    confirmCodecSwitch(): Promise<boolean> {
      if (!status.value?.recording) return Promise.resolve(true)
      codecSwitchPrompt.value?.(false)
      return new Promise(resolve => {
        codecSwitchPrompt.value = async confirmed => {
          codecSwitchPrompt.value = null
          // Finalize cleanly instead of letting the codec change abort the recording.
          if (confirmed) await stop()
          resolve(confirmed && !status.value?.recording)
        }
      })
    },
  }
}
