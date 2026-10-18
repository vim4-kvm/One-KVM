<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Switch } from '@/components/ui/switch'
import { Button } from '@/components/ui/button'
import SegmentedChoice from '@/components/SegmentedChoice.vue'
import { useUuRemoteContext } from './useUuRemote'
import type { UuRemoteStatusResponse } from '@/api'
import { request } from '@/api/request'
import { useConfigStore } from '@/stores/config'

const props = defineProps<{ isVim4: boolean; identityOnly?: boolean }>()
defineEmits<{ navigate: [section: 'video'] }>()
const { t } = useI18n()
const { response, busy, save } = useUuRemoteContext()
const store = useConfigStore()
const savedVideo = ref<UuRemoteStatusResponse['video']>(null)
// Like Vim4VideoSettings, prefer the running UU output over saved limits.
const video = computed(() => response.value?.video ?? savedVideo.value)
onMounted(async () => {
  if (!props.isVim4 || props.identityOnly || video.value) return
  try {
    const [settings, stream] = await Promise.all([
      request<{ output: { height: number; fps: number } }>('/config/video'),
      store.refreshStream(),
    ])
    const preset = stream.bitrate_preset
    savedVideo.value = {
      ...settings.output,
      bitrate_kbps:
        preset.type === 'Custom' ? preset.value : { Speed: 1000, Balanced: 4000, Quality: 8000 }[preset.type],
    }
  } catch (error) {
    errors.value.video = error instanceof Error ? error.message : t('api.operationFailedDesc')
  }
})
type Config = UuRemoteStatusResponse['config']
type Setting = 'system' | 'reverse_scroll' | 'video_codec'
const config = computed(() => response.value!.config)
const name = ref(config.value.device_name)
const focused = ref(false)
const dirty = ref(false)
const saved = ref(false)
const errors = ref<Record<string, string>>({})
const pending = ref<{ field: Setting; patch: Partial<Config> } | null>(null)
let savedTimer: ReturnType<typeof setTimeout>
const connected = computed(() => response.value?.status.sessions?.some((s) => s.state === 'connected'))
watch(
  () => config.value.device_name,
  (value) => {
    if (!focused.value && !dirty.value) name.value = value
  }
)
const systems = [
  { value: 'windows', label: 'Windows' },
  { value: 'macos', label: 'Mac' },
]
const codecs = computed(() => [
  { value: 'auto', label: t('extensions.uuRemote.codecAuto') },
  { value: 'h264', label: 'H.264' },
  { value: 'h265', label: 'H.265' },
])
const fields: Setting[] = ['system', 'reverse_scroll', 'video_codec']
const labels: Record<Setting, string> = { system: 'system', reverse_scroll: 'reverseScroll', video_codec: 'videoCodec' }
function value(field: Setting) {
  return pending.value?.patch[field] ?? config.value[field] ?? config.value.system === 'macos'
}
async function commit(field: string, patch: Partial<Config>) {
  errors.value[field] = ''
  try {
    await save(patch)
    return true
  } catch (error) {
    errors.value[field] = error instanceof Error ? error.message : t('api.operationFailedDesc')
    return false
  } finally {
    pending.value = null
  }
}
async function change(field: Setting, next: string | boolean) {
  if (next === value(field)) return
  const patch = { [field]: next } as Partial<Config>
  if (field === 'system') patch.reverse_scroll = next === 'macos'
  errors.value[field] = ''
  if (connected.value) pending.value = { field, patch }
  else await commit(field, patch)
}
async function saveName() {
  if (busy.value) return
  if (!dirty.value) {
    resetName()
    return
  }
  const trimmed = name.value.trim()
  if (!trimmed || [...trimmed].length > 64 || /\p{Cc}/u.test(trimmed)) {
    errors.value.name = t('extensions.uuRemote.invalidName')
    return
  }
  if (trimmed === config.value.device_name) {
    resetName()
    return
  }
  if (await commit('name', { device_name: trimmed })) {
    saved.value = true
    clearTimeout(savedTimer)
    savedTimer = setTimeout(() => {
      saved.value = false
    }, 1500)
  }
  name.value = config.value.device_name
  dirty.value = false
}
function resetName() {
  name.value = config.value.device_name
  dirty.value = false
  errors.value.name = ''
}
function editName() {
  dirty.value = true
  saved.value = false
  errors.value.name = ''
}
function blurName() {
  focused.value = false
  void saveName()
}
onBeforeUnmount(() => clearTimeout(savedTimer))
</script>

<template>
  <div class="space-y-4">
    <div v-if="!identityOnly" class="uu-setting-row">
      <Label for="uu-device-name">{{ t('extensions.uuRemote.deviceName') }}</Label>
      <div class="min-w-0 space-y-1">
        <div class="flex items-center gap-2">
          <Input
            id="uu-device-name"
            v-model="name"
            :disabled="busy"
            :aria-invalid="!!errors.name"
            aria-describedby="uu-name-error"
            @focus="focused = true"
            @input="editName"
            @blur="blurName"
            @keydown.enter.prevent="saveName"
            @keydown.esc="resetName"
          />
          <span v-if="saved" class="text-emerald-600" role="status" :aria-label="t('common.saved')">✓</span>
        </div>
        <p v-if="errors.name" id="uu-name-error" class="text-xs text-destructive" role="alert">{{ errors.name }}</p>
      </div>
    </div>
    <div v-for="field in identityOnly ? (['system'] as const) : fields" :key="field" class="uu-setting-row">
      <Label :for="field === 'reverse_scroll' ? 'uu-reverse-scroll' : undefined">
        {{ t(`extensions.uuRemote.${labels[field]}`) }}
      </Label>
      <div class="min-w-0 space-y-2">
        <div class="flex flex-wrap items-center gap-2">
          <Switch
            v-if="field === 'reverse_scroll'"
            id="uu-reverse-scroll"
            :model-value="Boolean(value(field))"
            :disabled="busy || !!pending"
            @update:model-value="change(field, $event)"
          />
          <SegmentedChoice
            v-else
            :model-value="String(value(field))"
            :label="t(`extensions.uuRemote.${labels[field]}`)"
            :options="field === 'system' ? systems : codecs"
            :disabled="busy || (!!pending && pending.field !== field)"
            @update:model-value="change(field, $event)"
          />
          <span v-if="field === 'reverse_scroll'" class="text-xs text-muted-foreground">
            {{ t('extensions.uuRemote.scrollDefault') }}
          </span>
          <span
            v-if="field === 'video_codec' && config.video_codec === 'auto' && connected && response?.status.video_codec"
            class="text-xs text-muted-foreground"
          >
            {{
              t('extensions.uuRemote.activeCodec', {
                codec: response.status.video_codec === 'h265' ? 'H.265' : 'H.264',
              })
            }}
          </span>
        </div>
        <p v-if="identityOnly" class="text-xs text-muted-foreground">{{ t('extensions.uuRemote.systemHint') }}</p>
        <div
          v-if="pending?.field === field"
          role="alert"
          class="space-y-2 rounded-md border border-amber-500/40 bg-amber-500/10 p-3 text-xs"
        >
          <p>
            {{ t('extensions.uuRemote.disconnectWarning') }}
            <span v-if="field === 'system'">{{ t('extensions.uuRemote.systemWarning') }}</span>
          </p>
          <div class="flex flex-wrap gap-2">
            <Button size="sm" :disabled="busy" @click="commit(field, pending!.patch)">
              {{ t('extensions.uuRemote.disconnectApply') }}
            </Button>
            <Button size="sm" variant="ghost" :disabled="busy" @click="pending = null">{{ t('common.cancel') }}</Button>
          </div>
        </div>
        <p v-if="errors[field]" class="text-xs text-destructive" role="alert">{{ errors[field] }}</p>
      </div>
    </div>
    <div v-if="isVim4 && !identityOnly" class="uu-setting-row">
      <Label>{{ t('extensions.uuRemote.videoLimit') }}</Label>
      <div class="flex flex-wrap items-center gap-x-3 gap-y-1 text-sm">
        <span v-if="video">
          {{ video.height ? `${video.height}p` : t('vim4Video.source') }} ·
          {{ video.fps ? `${video.fps} fps` : t('extensions.uuRemote.inputFps') }} ·
          {{ video.bitrate_kbps / 1000 }} Mbps
        </span>
        <Button variant="link" size="sm" class="h-auto whitespace-normal p-0" @click="$emit('navigate', 'video')">
          {{ t('extensions.uuRemote.adjustVideo') }} ›
        </Button>
        <p v-if="errors.video" class="text-xs text-destructive" role="alert">{{ errors.video }}</p>
      </div>
    </div>
  </div>
</template>

<style scoped>
.uu-setting-row {
  display: grid;
  gap: 0.5rem;
  align-items: start;
}
@media (min-width: 640px) {
  .uu-setting-row {
    grid-template-columns: 140px minmax(0, 1fr);
    align-items: center;
  }
}
</style>
