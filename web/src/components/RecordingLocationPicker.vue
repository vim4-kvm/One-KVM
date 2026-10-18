<script setup lang="ts">
import { computed, nextTick, onMounted, onUnmounted, ref, useId, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Check, HardDrive, Network } from 'lucide-vue-next'
import { Button } from '@/components/ui/button'
import { Spinner } from '@/components/ui/spinner'
import { formatBytes, useDeviceRecording, type RecordingLocation } from '@/composables/useDeviceRecording'

defineProps<{ inline?: boolean }>()
const { t } = useI18n()
const recording = useDeviceRecording()
const { status, locations, action } = recording
const id = useId()
const root = ref<HTMLElement>()
const trigger = ref<HTMLButtonElement>()
const selector = ref<HTMLElement>()
const open = ref(false)
const loading = ref(false)
const selectedLocation = ref('local')
const folder = ref('One-KVM')
const locationError = ref('')
const folderError = ref('')
const folderSaved = ref(false)
const savingFolder = ref(false)
let pendingFolderSave: Promise<void> | undefined
let savedTimer: ReturnType<typeof setTimeout> | undefined
let disposed = false
let restoreTriggerFocus = false
const managerStorageUrl = `http://${window.location.hostname}/#/storage`
const currentLocation = computed(() => status.value?.location)
const unavailableReason = computed(() => status.value?.storage_reason)
const busy = computed(() => !!action.value || !!status.value?.recording || loading.value)
const selectedAvailable = computed(() => !!locations.value.find(location => location.id === selectedLocation.value)?.available)
const locationLabel = computed(() => {
  const location = currentLocation.value
  if (!location) return t('recording.loading')
  return location.id === 'local' ? locationName(location) : `${locationName(location)} › ${location.folder}`
})

function locationName(location: RecordingLocation) {
  return location.id === 'local' ? t('recording.local') : location.name
}

function syncSelection() {
  selectedLocation.value = currentLocation.value?.id ?? 'local'
  folder.value = currentLocation.value?.folder || 'One-KVM'
}

watch(currentLocation, () => {
  if (!open.value && !savingFolder.value) syncSelection()
})
watch(() => status.value?.recording, active => { if (active) close() })
watch(action, () => { if (!action.value) returnFocus() })
watch(folder, () => {
  folderError.value = ''
  folderSaved.value = false
  clearTimeout(savedTimer)
})

async function toggle() {
  if (open.value) { close(); return }
  restoreTriggerFocus = false
  if (!folderError.value) syncSelection()
  locationError.value = ''
  folderSaved.value = false
  open.value = true
  loading.value = true
  await nextTick()
  selector.value?.focus()
  try {
    await recording.refreshLocations()
  } catch {
    locationError.value = t('recording.locationsFailed')
  } finally {
    loading.value = false
    await nextTick()
    if (open.value && document.activeElement === selector.value) {
      selector.value?.querySelector<HTMLInputElement>('input[type="radio"]:not(:disabled)')?.focus()
    }
  }
}

function close() {
  if (!open.value) return
  // Removing a focused input does not consistently emit blur in browsers.
  void saveFolder()
  open.value = false
  restoreTriggerFocus = true
  returnFocus()
}

function returnFocus() {
  // The trigger remains disabled until an in-flight location write completes.
  if (!restoreTriggerFocus || action.value || open.value) return
  void nextTick(() => {
    if (!open.value && !action.value) {
      trigger.value?.focus()
      restoreTriggerFocus = false
    }
  })
}

async function finish() {
  await saveFolder()
  if (!folderError.value) close()
}

async function selectLocation(location: RecordingLocation) {
  // A folder blur can start saving just before the next radio's change event.
  // Finish that write before changing destinations so each NAS keeps its folder.
  if (pendingFolderSave) await pendingFolderSave
  if (busy.value || selectedLocation.value === location.id) return
  const previous = selectedLocation.value
  const previousFolder = folder.value
  selectedLocation.value = location.id
  folder.value = location.folder || 'One-KVM'
  locationError.value = ''
  folderError.value = ''
  try {
    await recording.setLocation(location.id, folder.value)
  } catch {
    selectedLocation.value = previous
    folder.value = previousFolder
    locationError.value = t('recording.locationSaveFailed')
  } finally {
    await nextTick()
    if (open.value && document.activeElement === document.body) {
      selector.value?.querySelector<HTMLInputElement>('input[type="radio"]:checked:not(:disabled)')?.focus()
    }
  }
}

function saveFolder(): Promise<void> {
  if (pendingFolderSave) return pendingFolderSave
  pendingFolderSave = persistFolder().finally(() => { pendingFolderSave = undefined })
  return pendingFolderSave
}

async function persistFolder() {
  if (busy.value || selectedLocation.value === 'local' || !selectedAvailable.value) return
  if (folder.value === currentLocation.value?.folder) return
  // Match the server's relative POSIX path rules, including UTF-8 byte length.
  if (!folder.value || new TextEncoder().encode(folder.value).length > 4096
    || /[\u0000-\u001f\u007f-\u009f]/.test(folder.value)
    || folder.value.split('/').some(part => !part || part === '.' || part === '..')) {
    folderError.value = t('recording.invalidFolder')
    return
  }
  folderError.value = ''
  savingFolder.value = true
  try {
    await recording.setLocation(selectedLocation.value, folder.value)
    if (!disposed) {
      folderSaved.value = true
      savedTimer = setTimeout(() => { folderSaved.value = false }, 2000)
    }
  } catch {
    folderError.value = t('recording.folderSaveFailed')
    // Keep a failed blur-save visible even if Done or an outside click closed the picker.
    if (!disposed) open.value = true
  } finally {
    savingFolder.value = false
  }
}

function onPointerDown(event: PointerEvent) {
  if (open.value && event.target instanceof Node && !root.value?.contains(event.target)) close()
}
function onKeyDown(event: KeyboardEvent) {
  if (open.value && event.key === 'Escape') {
    // Consume Escape before the surrounding recording popover or bottom sheet.
    event.preventDefault()
    event.stopPropagation()
    close()
  }
}
onMounted(() => {
  document.addEventListener('pointerdown', onPointerDown, true)
  window.addEventListener('keydown', onKeyDown, true)
})
onUnmounted(() => {
  disposed = true
  clearTimeout(savedTimer)
  document.removeEventListener('pointerdown', onPointerDown, true)
  window.removeEventListener('keydown', onKeyDown, true)
})
</script>

<template>
  <div ref="root" class="relative -mx-4 mb-3 border-b px-4 pb-3 text-xs">
    <div :class="['flex min-w-0 items-center gap-2', unavailableReason && 'text-amber-700 dark:text-amber-400']">
      <span class="shrink-0">{{ t('recording.saveTo') }}</span>
      <span :class="['size-1.5 shrink-0 rounded-full', unavailableReason === 'unresponsive' ? 'bg-amber-500' : unavailableReason || !status ? 'bg-muted-foreground' : 'bg-green-500']" aria-hidden="true" />
      <span class="sr-only">{{ unavailableReason ? t(`recording.locationReasons.${unavailableReason}`) : status ? t('recording.locationAvailable') : t('recording.loading') }}</span>
      <component :is="currentLocation?.id === 'local' ? HardDrive : Network" class="size-3.5 shrink-0" aria-hidden="true" />
      <span class="min-w-0 flex-1 truncate" :title="locationLabel">{{ locationLabel }}</span>
      <span v-if="unavailableReason" class="max-w-[35%] shrink-0 truncate" :title="t(`recording.locationReasons.${unavailableReason}`)">· {{ t(`recording.locationReasons.${unavailableReason}`) }}</span>
      <button ref="trigger" type="button" class="shrink-0 rounded px-1 py-1 font-medium hover:bg-muted focus-visible:outline-ring disabled:opacity-50"
        :disabled="!!status?.recording || !!action" :title="status?.recording ? t('recording.locationLocked') : undefined"
        :aria-expanded="open" :aria-controls="id" @click="toggle">{{ t('recording.changeLocation') }}</button>
    </div>
    <div v-if="open" :id="id" ref="selector" tabindex="-1" :aria-label="t('recording.saveTo')"
      :class="['min-w-0 space-y-3 rounded-lg border bg-popover p-3 text-popover-foreground outline-none', inline ? 'mt-3' : 'absolute inset-x-0 top-full z-10 shadow-lg']">
      <div v-if="loading" class="flex items-center gap-2 text-muted-foreground" role="status"><Spinner class="size-3.5" />{{ t('recording.loadingLocations') }}</div>
      <div role="radiogroup" :aria-label="t('recording.saveTo')" :aria-busy="loading || action === 'location'" class="space-y-1">
        <label v-for="location in locations" :key="location.id" :class="['flex min-w-0 items-center gap-2 rounded-md px-1 py-2', location.available ? 'hover:bg-muted' : 'text-muted-foreground']">
          <input type="radio" :name="id" class="shrink-0" :value="location.id" :checked="selectedLocation === location.id"
            :disabled="(busy && !savingFolder) || !location.available" @change="selectLocation(location)">
          <span class="min-w-0 flex-1 truncate" :title="locationName(location)">{{ locationName(location) }}</span>
          <Spinner v-if="action === 'location' && selectedLocation === location.id" class="size-3.5 shrink-0" />
          <span class="shrink-0 whitespace-nowrap text-right text-muted-foreground">
            {{ !location.available ? t(`recording.locationReasons.${location.reason ?? 'not_writable'}`) : location.free_bytes != null ? t('recording.free', { size: formatBytes(location.free_bytes) }) : '' }}
          </span>
        </label>
      </div>
      <p v-if="locationError" role="alert" class="text-xs text-destructive">{{ locationError }}</p>
      <div v-if="selectedLocation !== 'local'" class="space-y-1 border-t pt-3">
        <div class="flex min-w-0 items-center gap-2">
          <label :for="`${id}-folder`" class="shrink-0">{{ t('recording.folder') }}</label>
          <input :id="`${id}-folder`" v-model="folder" class="min-w-0 flex-1 rounded-md border bg-background px-2 py-1.5"
            :disabled="(busy && !savingFolder) || !selectedAvailable" :readonly="savingFolder" :aria-invalid="!!folderError" :aria-describedby="folderError ? `${id}-error` : undefined"
            placeholder="One-KVM" @keydown.enter.prevent="saveFolder" @blur="saveFolder">
          <span class="inline-flex size-4 shrink-0" role="status"><Check v-if="folderSaved" class="size-4 text-green-600 dark:text-green-400" :aria-label="t('recording.folderSaved')" /></span>
        </div>
        <p v-if="folderError" :id="`${id}-error`" role="alert" class="text-xs text-destructive">{{ folderError }}</p>
      </div>
      <div class="flex items-center justify-between gap-2">
        <a :href="managerStorageUrl" target="_blank" rel="noopener" class="text-muted-foreground underline">{{ t('recording.manageNas') }} ↗</a>
        <Button size="sm" variant="outline" @click="finish">{{ t('recording.done') }}</Button>
      </div>
    </div>
  </div>
</template>
