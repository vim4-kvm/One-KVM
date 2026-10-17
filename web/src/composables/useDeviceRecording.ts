import { computed, ref } from 'vue'
import { toast } from 'vue-sonner'
import i18n from '@/i18n'
import { request } from '@/api/request'

export type RecordingCode =
  | 'already_recording' | 'video_codec' | 'audio_unavailable' | 'low_storage' | 'storage_error'
  | 'video_unavailable' | 'start_timeout' | 'codec_changed' | 'resolution_changed' | 'packets_lost'
  | 'capture_timeout' | 'pipeline_stopped' | 'save_failed' | 'internal'

export interface RecordingStatus {
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
const unreachable = ref(false)
const action = ref<'starting' | 'stopping' | 'deleting' | null>(null)
const panelOpen = ref(false)
const fetchedAt = ref(0)
const now = ref(Date.now())
const openRequests = ref(0)
const codecSwitchPrompt = ref<((confirmed: boolean) => void) | null>(null)

let users = 0
let pollTimer: ReturnType<typeof setTimeout> | undefined
let clockTimer: ReturnType<typeof setInterval> | undefined
let stopRequested = false

function t(key: string, params?: Record<string, unknown>) {
  return String(i18n.global.t(key, params as never))
}

export function localDownloadName(file: RecordingFile) {
  const date = new Date((file.started_at ?? file.modified) * 1000)
  const pad = (value: number) => value.toString().padStart(2, '0')
  return `one-kvm-${date.getFullYear()}${pad(date.getMonth() + 1)}${pad(date.getDate())}-${pad(date.getHours())}${pad(date.getMinutes())}${pad(date.getSeconds())}.mkv`
}

export function recordingDownloadUrl(file: RecordingFile) {
  return `/api/recording/files/${encodeURIComponent(file.name)}?filename=${localDownloadName(file)}`
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
  files.value = await request<RecordingFile[]>('/recording/files', {}, { toastOnError: false })
}

function announceTransition(previous: RecordingStatus | null, next: RecordingStatus) {
  if (!previous?.recording || next.recording) return
  const byUser = stopRequested
  stopRequested = false
  const saved = files.value.find(file => file.name === next.file && file.complete)
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
    const next = await request<RecordingStatus>('/recording', {}, { toastOnError: false })
    const finished = previous?.recording && !next.recording
    if (panelOpen.value || finished || !files.value.length) await fetchFiles()
    status.value = next
    fetchedAt.value = Date.now()
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
  await request(`/recording/files/${encodeURIComponent(file.name)}`, { method: 'DELETE' })
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
  if (!storage || !bitrateBps.value) return null
  return Math.max(0, storage.free_bytes - storage.reserve_bytes) * 8 / bitrateBps.value
})

export function useDeviceRecording() {
  return {
    status,
    files,
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
