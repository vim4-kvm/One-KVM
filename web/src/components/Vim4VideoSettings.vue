<script setup lang="ts">
import { computed, onMounted, ref } from 'vue'
import { useI18n } from 'vue-i18n'
import { toast } from 'vue-sonner'
import { request } from '@/api/request'
import { streamApi, uuRemoteApi } from '@/api'
import { useConfigStore } from '@/stores/config'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'

interface OutputSettings { height: number; fps: number; hevc_quality?: { type: string; qp?: number } }
interface BoardStatus { edid: string; hevc_quality_available: boolean; hevc_lossless_available: boolean }
const props = defineProps<{ outputOnly?: boolean }>()
const { t } = useI18n()
const store = useConfigStore()
const height = ref('0')
const fps = ref('0')
const hevcMode = ref('bitrate')
const hevcQp = ref(28)
const quality = ref(80)
const bitrate = ref(4000)
const board = ref<BoardStatus | null>(null)
const edid = ref('VIM4-4K60')
const ready = ref(false)
const busy = ref(false)
const heights = computed(() => [...new Set([720, 900, 1080, 1440, 2160, Number(height.value)])].filter(v => v > 0).sort((a, b) => a - b))
const frameRates = computed(() => [...new Set([5, 10, 15, 24, 30, 60, 100, 120, Number(fps.value)])].filter(v => v > 0).sort((a, b) => a - b))

async function load() {
  ready.value = false
  try {
    const [video, status] = await Promise.all([
      request<{ output: OutputSettings; quality: number }>('/config/video'),
      request<BoardStatus>('/vim4/video'),
      store.refreshStream(),
    ])
    height.value = String(video.output.height)
    fps.value = String(video.output.fps)
    hevcMode.value = video.output.hevc_quality?.type ?? 'bitrate'
    hevcQp.value = video.output.hevc_quality?.qp ?? 28
    quality.value = video.quality
    board.value = status
    edid.value = status.edid
    const preset = store.stream?.bitrate_preset
    bitrate.value = preset?.type === 'Custom' ? preset.value : ({ Speed: 1000, Balanced: 4000, Quality: 8000 }[preset?.type ?? 'Balanced'])
    const runtime = (await uuRemoteApi.getStatus()).video
    if (runtime) {
      height.value = String(runtime.height)
      fps.value = String(runtime.fps)
      bitrate.value = runtime.bitrate_kbps
    }
    ready.value = true
  } catch { /* request displays the error and leaves controls disabled */ }
}

async function apply(action: 'output' | 'bitrate' | 'edid') {
  busy.value = true
  try {
    if (action === 'output') {
      await request('/config/video', { method: 'PATCH', body: JSON.stringify({
        output: { height: Number(height.value), fps: Number(fps.value), hevc_quality: {
          type: hevcMode.value, ...(hevcMode.value === 'fixed_qp' ? { qp: Number(hevcQp.value) } : {}),
        } }, quality: Number(quality.value),
      }) })
      await store.refreshVideo()
    } else if (action === 'bitrate') {
      await streamApi.setBitratePreset({ type: 'Custom', value: Number(bitrate.value) })
      await store.refreshStream()
    } else {
      board.value = await request<BoardStatus>('/vim4/video', { method: 'POST',
        body: JSON.stringify({ action, preset: edid.value }),
      })
    }
    toast.success(t('vim4Video.applied'))
  } catch { /* request displays the error */ }
  finally { busy.value = false }
}
onMounted(load)
</script>

<template>
  <div class="space-y-4 text-xs" data-testid="vim4-video-settings">
    <div class="flex items-center justify-between">
      <h5 class="font-medium">{{ t('vim4Video.title') }}</h5>
      <Button variant="ghost" size="sm" :disabled="busy" @click="load">{{ t('common.refresh') }}</Button>
    </div>
    <fieldset :disabled="!ready || busy" class="space-y-4 disabled:opacity-60">
      <form class="space-y-2" @submit.prevent="apply('output')">
        <Label>{{ t('vim4Video.outputSize') }}</Label>
        <Select v-model="height" :disabled="!ready || busy">
          <SelectTrigger class="w-full"><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="0">{{ t('vim4Video.source') }}</SelectItem>
            <SelectItem v-for="value in heights" :key="value" :value="String(value)">{{ value }}p</SelectItem>
          </SelectContent>
        </Select>
        <Label>{{ t('vim4Video.outputFps') }}</Label>
        <Select v-model="fps" :disabled="!ready || busy">
          <SelectTrigger class="w-full"><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="0">{{ t('vim4Video.source') }}</SelectItem>
            <SelectItem v-for="value in frameRates" :key="value" :value="String(value)">{{ value }} fps</SelectItem>
          </SelectContent>
        </Select>
        <template v-if="!props.outputOnly">
          <Label for="vim4-quality">{{ t('vim4Video.jpegQuality') }}</Label>
          <Input id="vim4-quality" v-model="quality" type="number" min="1" max="100" required />
        </template>
        <p class="text-muted-foreground">{{ t('vim4Video.outputHint') }}</p>
        <Label>{{ t('vim4Video.hevcMode') }}</Label>
        <Select v-model="hevcMode" :disabled="!ready || busy">
          <SelectTrigger class="w-full"><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="bitrate">{{ t('vim4Video.hevcBitrate') }}</SelectItem>
            <SelectItem value="fixed_qp" :disabled="!board?.hevc_quality_available">{{ t('vim4Video.hevcFixedQp') }}</SelectItem>
            <SelectItem value="lossless" :disabled="!board?.hevc_lossless_available">{{ t('vim4Video.hevcLossless') }}</SelectItem>
          </SelectContent>
        </Select>
        <Input v-if="hevcMode === 'fixed_qp'" v-model="hevcQp" aria-label="HEVC QP" type="number" min="0" max="51" required />
        <p class="text-muted-foreground">{{ t('vim4Video.hevcHint') }}</p>
        <Button type="submit" size="sm" class="w-full">{{ t('common.apply') }}</Button>
      </form>
      <form class="space-y-2" @submit.prevent="apply('bitrate')">
        <Label for="vim4-bitrate">{{ t('vim4Video.bitrate') }}</Label>
        <Input id="vim4-bitrate" v-model="bitrate" type="number" min="1000" max="60000" required />
        <Button type="submit" size="sm" variant="outline" class="w-full">{{ t('common.apply') }}</Button>
      </form>
      <form v-if="!props.outputOnly" class="space-y-2" @submit.prevent="apply('edid')">
        <Label>EDID</Label>
        <Select v-model="edid" :disabled="!ready || busy">
          <SelectTrigger class="w-full"><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="VIM4-4K60">4K 60 Hz</SelectItem>
            <SelectItem value="RTK-CONTROL">RTK UHD HDR</SelectItem>
            <SelectItem value="VIM4-1080P60">1080p 60 Hz</SelectItem>
            <SelectItem value="VIM4-1080P60-SDR8">1080p 60 Hz · SDR 8-bit</SelectItem>
            <SelectItem v-if="board && !['VIM4-4K60', 'RTK-CONTROL', 'RTK-1080P120', 'VIM4-1080P60', 'VIM4-1080P60-SDR8'].includes(board.edid)" :value="board.edid" disabled>{{ board.edid }}</SelectItem>
          </SelectContent>
        </Select>
        <p class="text-muted-foreground">{{ t('vim4Video.edidHint') }}</p>
        <Button type="submit" size="sm" variant="outline" class="w-full">{{ t('common.apply') }}</Button>
      </form>
    </fieldset>
  </div>
</template>
