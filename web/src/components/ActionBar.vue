<script setup lang="ts">
import { ref, computed, onMounted, onUnmounted, watch, nextTick } from 'vue'
import { useI18n } from 'vue-i18n'
import { useRouter } from 'vue-router'
import { useSystemStore } from '@/stores/system'
import type { VideoRotation, VideoScaleMode } from '@/composables/useVideoScaling'
import { Button } from '@/components/ui/button'
import { ButtonGroup } from '@/components/ui/button-group'
import {
  PopoverContent,
  PopoverTrigger,
  Popover,
} from '@/components/ui/popover'
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from '@/components/ui/tooltip'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import {
  Sheet,
  SheetContent,
  SheetHeader,
  SheetTitle,
} from '@/components/ui/sheet'
import {
  ClipboardPaste,
  CircleDot,
  HardDrive,
  Settings,
  Power,
  BarChart3,
  Terminal,
  MoreHorizontal,
  Bot,
  Keyboard,
  Scaling,
} from 'lucide-vue-next'
import PasteModal from '@/components/PasteModal.vue'
import RecordingPanel from '@/components/RecordingPanel.vue'
import { useDeviceRecording } from '@/composables/useDeviceRecording'
import AtxPopover from '@/components/AtxPopover.vue'
import VideoConfigPopover, { type VideoMode } from '@/components/VideoConfigPopover.vue'
import HidConfigPopover from '@/components/HidConfigPopover.vue'
import AudioConfigPopover from '@/components/AudioConfigPopover.vue'
import MsdDialog from '@/components/MsdDialog.vue'
import VideoDisplayControls from '@/components/VideoDisplayControls.vue'
import type { ConsoleLayout } from '@/composables/useConsoleLayout'

const { t, locale } = useI18n()
const router = useRouter()
const systemStore = useSystemStore()

const overflowMenuOpen = defineModel<boolean>('moreOpen', { default: false })

const hidBackend = computed(() => (systemStore.hid?.backend ?? '').toLowerCase())
const isCh9329Backend = computed(() => hidBackend.value.includes('ch9329'))
const showMsd = computed(() => {
  return !!systemStore.msd?.available && !isCh9329Backend.value
})
const props = defineProps<{
  layout?: ConsoleLayout
  isFullscreen?: boolean
  mouseMode?: 'absolute' | 'relative'
  videoMode?: VideoMode
  videoRotation?: VideoRotation
  ttydRunning?: boolean
  showPower?: boolean
  atxEnabled?: boolean
  showTerminal?: boolean
  terminalOpen?: boolean
  statsOpen?: boolean
  showComputerUse?: boolean
  showPasteText?: boolean
  showRecording?: boolean
  showMic?: boolean
  scaleMode?: VideoScaleMode
  sourceSizeAvailable?: boolean
}>()
const isSidebarLayout = computed(() => props.layout === 'sidebar')
const showAtx = computed(() => props.showPower !== false)
const atxEnabled = computed(() => props.atxEnabled === true)
const showStats = computed(() => (props.videoMode ?? 'mjpeg') !== 'mjpeg')
const showPasteText = computed(() => props.showPasteText !== false)
const showRecording = computed(() => props.showRecording === true)
const deviceRecording = useDeviceRecording()
const isRecording = computed(() => !!deviceRecording.status.value?.recording)
const showMic = computed(() => props.showMic === true)


const emit = defineEmits<{
  (e: 'naturalWidth', width: number): void
  (e: 'toggleFullscreen'): void
  (e: 'update:scaleMode', mode: VideoScaleMode): void
  (e: 'toggleStats'): void
  (e: 'toggleVirtualKeyboard'): void
  (e: 'toggleMouseMode'): void
  (e: 'update:videoMode', mode: VideoMode): void
  (e: 'update:videoRotation', rotation: VideoRotation): void
  (e: 'powerShort'): void
  (e: 'powerLong'): void
  (e: 'reset'): void
  (e: 'wol', macAddress: string): void
  (e: 'openTerminal'): void
  (e: 'openComputerUse'): void
}>()

const pasteOpen = ref(false)
const recordOpen = ref(false)
const mobileRecordOpen = ref(false)
const mobileRecordOpenTime = ref(0)
const atxOpen = ref(false)
const videoPopoverOpen = ref(false)
const hidPopoverOpen = ref(false)
const audioPopoverOpen = ref(false)
const msdDialogOpen = ref(false)

const mobileAtxOpen = ref(false)
const mobilePasteOpen = ref(false)
const mobileAtxOpenTime = ref(0)
const mobilePasteOpenTime = ref(0)

const OPEN_GUARD_MS = 350

const guardOutside = (openTime: number, e: Event) => {
  if (Date.now() - openTime < OPEN_GUARD_MS) {
    e.preventDefault()
  }
}

const openFromOverflow = (setter: () => void) => {
  overflowMenuOpen.value = false
  setTimeout(setter, 50)
}

const openMobileAtx = () => openFromOverflow(() => {
  if (!showAtx.value) return
  mobileAtxOpen.value = true
  mobileAtxOpenTime.value = Date.now()
})

const openMobileRecord = () => openFromOverflow(() => {
  mobileRecordOpen.value = true
  mobileRecordOpenTime.value = Date.now()
})

// Confirmation dialogs are portaled outside the popover; interacting with them must not close it.
const keepOpenForDialogs = (e: Event) => {
  const target = (e as CustomEvent<{ originalEvent?: Event }>).detail?.originalEvent?.target ?? e.target
  if (target instanceof Element && target.closest('[role="alertdialog"]')) e.preventDefault()
}

const openMobilePaste = () => openFromOverflow(() => {
  if (!showPasteText.value) return
  mobilePasteOpen.value = true
  mobilePasteOpenTime.value = Date.now()
})


const barRef = ref<HTMLElement | null>(null)
const measureRef = ref<HTMLElement | null>(null)
const barWidth = ref(0)
const minimalDisplayControls = computed(() => !isSidebarLayout.value && barWidth.value < 360)
let layoutResizeObserver: ResizeObserver | null = null

type CollapsibleItem =
  | 'msd' | 'atx' | 'paste' | 'record'
  | 'stats' | 'terminal' | 'settings' | 'ai'

interface ItemSpec {
  id: CollapsibleItem
  side: 'left' | 'right'
}

const ITEM_SPECS: ItemSpec[] = [
  { id: 'msd',       side: 'left' },
  { id: 'atx',       side: 'left' },
  { id: 'paste',     side: 'left' },
  { id: 'record',    side: 'left' },
  { id: 'stats',     side: 'right' },
  { id: 'terminal',  side: 'right' },
  { id: 'settings',  side: 'right' },
  { id: 'ai',        side: 'right' },
]

const measuredWidths = ref<Map<CollapsibleItem, { icon: number; label: number }>>(new Map())
const measurementReady = ref(false)
const coreLabelWidth = ref(0)
const displayLabelWidth = ref(0)

const measureLayout = async () => {
  await nextTick()
  const bar = barRef.value
  const measureContainer = measureRef.value
  if (!bar || !measureContainer) return

  const style = window.getComputedStyle(bar)
  barWidth.value = bar.clientWidth - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight)

  const newWidths = new Map<CollapsibleItem, { icon: number; label: number }>()
  for (const spec of ITEM_SPECS) {
    const iconEl = measureContainer.querySelector(`[data-measure="${spec.id}-icon"]`) as HTMLElement
    const labelEl = measureContainer.querySelector(`[data-measure="${spec.id}-label"]`) as HTMLElement
    if (iconEl && labelEl) {
      newWidths.set(spec.id, {
        icon: Math.ceil(iconEl.offsetWidth) + 8,
        label: Math.ceil(labelEl.offsetWidth) + 8,
      })
    }
  }
  measuredWidths.value = newWidths
  const measured = (id: string) => (measureContainer.querySelector('[data-measure="' + id + '-label"]') as HTMLElement)?.offsetWidth ?? 0
  coreLabelWidth.value = ['video', 'audio', 'hid'].reduce((sum, id) => sum + measured(id), 0)
  displayLabelWidth.value = measured('fullscreen') + measured('keyboard') + 57
  const naturalWidth = coreLabelWidth.value + displayLabelWidth.value
    + collapsibleItems.value.reduce((sum, item) => sum + (newWidths.get(item.id)?.label ?? 0), 0) + 56
  emit('naturalWidth', naturalWidth)

  measurementReady.value = true
}

const observeLayout = async () => {
  await measureLayout()
  layoutResizeObserver?.disconnect()
  layoutResizeObserver = new ResizeObserver(() => {
    void measureLayout()
  })
  if (barRef.value) layoutResizeObserver.observe(barRef.value)
  barRef.value?.querySelectorAll('[data-fixed-action], [data-core-action]').forEach((element) => {
    layoutResizeObserver?.observe(element)
  })
}

onMounted(() => {
  void observeLayout()
})

onUnmounted(() => {
  layoutResizeObserver?.disconnect()
})

watch(locale, () => {
  measurementReady.value = false
  void measureLayout()
})

watch([() => props.layout, () => props.showComputerUse, minimalDisplayControls], () => {
  // Layout changes can move this bar while its portal based controls are open.
  // Close every transient surface first so no old portal remains attached to
  // the document after the new layout is painted.
  overflowMenuOpen.value = false
  pasteOpen.value = false
  recordOpen.value = false
  mobileRecordOpen.value = false
  atxOpen.value = false
  videoPopoverOpen.value = false
  hidPopoverOpen.value = false
  audioPopoverOpen.value = false
  msdDialogOpen.value = false
  mobileAtxOpen.value = false
  mobilePasteOpen.value = false
  void observeLayout()
})

watch(showAtx, (visible) => {
  if (!visible) {
    atxOpen.value = false
    mobileAtxOpen.value = false
  }
})

watch(showPasteText, (visible) => {
  if (!visible) {
    pasteOpen.value = false
    mobilePasteOpen.value = false
  }
})

watch(deviceRecording.openRequests, async () => {
  if (!showRecording.value) return
  await nextTick()
  await measureLayout()
  if (isVisible('record')) recordOpen.value = true
  else {
    mobileRecordOpen.value = true
    mobileRecordOpenTime.value = Date.now()
  }
})


const collapsibleItems = computed(() => {
  const items = ITEM_SPECS.filter(item => {
    if (item.id === 'msd' && !showMsd.value) return false
    if (item.id === 'atx' && !showAtx.value) return false
    if (item.id === 'paste' && !showPasteText.value) return false
    if (item.id === 'record' && !showRecording.value) return false
    if (item.id === 'stats' && !showStats.value) return false
    if (item.id === 'terminal' && props.showTerminal === false) return false
    if (item.id === 'ai' && props.showComputerUse === false) return false
    return true
  })
  return items
})

const visibleSet = computed(() => {
  const result = new Map<CollapsibleItem, 'icon' | 'label'>()
  if (isSidebarLayout.value) {
    // The rail scrolls vertically; every enabled action stays in the column.
    for (const item of collapsibleItems.value) result.set(item.id, 'icon')
    return result
  }
  if (!measurementReady.value) return result

  const allLabelsWidth = coreLabelWidth.value + displayLabelWidth.value
    + collapsibleItems.value.reduce((sum, item) => sum + (measuredWidths.value.get(item.id)?.label ?? 0), 0) + 56
  if (allLabelsWidth <= barWidth.value) {
    for (const item of collapsibleItems.value) result.set(item.id, 'label')
    return result
  }
  // Preserve the right group first; overflow the end of the left group first.
  let available = barWidth.value - 96 - (minimalDisplayControls.value ? 32 : 121) - 48
  const priority = [...collapsibleItems.value.filter(item => item.side === 'right'),
    ...collapsibleItems.value.filter(item => item.side === 'left')]
  for (const item of priority) {
    const width = measuredWidths.value.get(item.id)?.icon ?? 40
    if (available < width) continue
    result.set(item.id, 'icon')
    available -= width
  }
  return result
})

const labelsVisible = computed(() => !isSidebarLayout.value && measurementReady.value
  && coreLabelWidth.value + displayLabelWidth.value
    + collapsibleItems.value.reduce((sum, item) => sum + (measuredWidths.value.get(item.id)?.label ?? 0), 0) + 56 <= barWidth.value)
const isVisible = (id: CollapsibleItem) => visibleSet.value.has(id)
const hasOverflow = computed(() => {
  return collapsibleItems.value.some(i => !visibleSet.value.has(i.id))
})
const hasLeftOverflow = computed(() => {
  return collapsibleItems.value.some(i => i.side === 'left' && !visibleSet.value.has(i.id))
})
const hasRightOverflow = computed(() => {
  return collapsibleItems.value.some(i => i.side === 'right' && !visibleSet.value.has(i.id))
})

const hasOpenSurface = computed(() => overflowMenuOpen.value || pasteOpen.value || recordOpen.value
  || mobileRecordOpen.value || atxOpen.value || videoPopoverOpen.value || hidPopoverOpen.value
  || audioPopoverOpen.value || msdDialogOpen.value || mobileAtxOpen.value || mobilePasteOpen.value)
defineExpose({ hasOpenSurface })
</script>

<template>
  <div
    :data-label-mode="labelsVisible ? 'labels' : 'icons'"
    :class="[
      'console-action-bar',
      props.layout === 'sidebar' && 'console-action-bar--sidebar',
      (!props.layout || props.layout === 'current') && 'w-full',
    ]"
  >
    <div
      ref="barRef"
      class="console-action-bar__inner flex items-center"
      :class="isSidebarLayout
        ? 'h-full flex-col px-1.5 py-2'
        : 'h-8 p-0'"
    >
      <!-- Left side buttons -->
      <ButtonGroup
        class="left-buttons min-w-0"
        :class="isSidebarLayout
          ? 'flex-none flex-col overflow-visible'
          : 'flex-1 overflow-hidden'"
        :orientation="isSidebarLayout ? 'vertical' : 'horizontal'"
      >
        <!-- Video Config - Always visible -->
        <div data-core-action class="flex shrink-0">
          <VideoConfigPopover
            v-model:open="videoPopoverOpen"
            :video-mode="props.videoMode || 'mjpeg'"
            :video-rotation="props.videoRotation ?? 0"
            :side="isSidebarLayout ? 'right' : 'bottom'"
            @update:video-mode="emit('update:videoMode', $event)"
            @update:video-rotation="emit('update:videoRotation', $event)"
          />
        </div>

        <!-- Audio Config - Always visible -->
        <div data-core-action class="flex shrink-0">
          <AudioConfigPopover
            v-model:open="audioPopoverOpen"
            :microphone-enabled="showMic"
            :side="isSidebarLayout ? 'right' : 'bottom'"
          />
        </div>

        <!-- HID Config - Always visible -->
        <div data-core-action class="flex shrink-0">
          <HidConfigPopover
            v-model:open="hidPopoverOpen"
            :mouse-mode="mouseMode"
            :side="isSidebarLayout ? 'right' : 'bottom'"
            @update:mouse-mode="emit('toggleMouseMode')"
          />
        </div>

        <!-- Virtual Media (MSD) - Adaptive -->
        <div v-if="showMsd && isVisible('msd')">
          <TooltipProvider>
            <Tooltip>
              <TooltipTrigger as-child>
                <Button
                  variant="ghost"
                  size="sm"
                  class="h-8 gap-1.5 text-xs"
                  :aria-label="t('actionbar.virtualMedia')"
                  :title="t('actionbar.virtualMedia')"
                  @click="msdDialogOpen = true"
                >
                  <HardDrive class="size-4" />
                  <span data-action-label>{{ t('actionbar.virtualMedia') }}</span>
                </Button>
              </TooltipTrigger>
              <TooltipContent>
                <p>{{ t('actionbar.virtualMediaTip') }}</p>
              </TooltipContent>
            </Tooltip>
          </TooltipProvider>
        </div>

        <!-- ATX Power Control - Adaptive -->
        <div v-if="showAtx && isVisible('atx')">
          <Popover v-model:open="atxOpen">
            <PopoverTrigger as-child>
              <Button
                variant="ghost"
                size="sm"
                class="h-8 gap-1.5 text-xs"
                :aria-label="t('actionbar.power')"
                :title="t('actionbar.power')"
              >
                <Power class="size-4" />
                <span data-action-label>{{ t('actionbar.power') }}</span>
              </Button>
            </PopoverTrigger>
            <PopoverContent
              class="w-[min(280px,90vw)] p-0"
              align="start"
              :side="isSidebarLayout ? 'right' : 'bottom'"
            >
              <AtxPopover
                :atx-enabled="atxEnabled"
                @close="atxOpen = false"
                @power-short="emit('powerShort')"
                @power-long="emit('powerLong')"
                @reset="emit('reset')"
                @wol="(mac) => emit('wol', mac)"
              />
            </PopoverContent>
          </Popover>
        </div>

        <!-- Paste Text - Adaptive -->
        <div v-if="showPasteText && isVisible('paste')">
          <Popover v-model:open="pasteOpen">
            <PopoverTrigger as-child>
              <Button
                variant="ghost"
                size="sm"
                class="h-8 gap-1.5 text-xs"
                :aria-label="t('actionbar.paste')"
                :title="t('actionbar.paste')"
              >
                <ClipboardPaste class="size-4" />
                <span data-action-label>{{ t('actionbar.paste') }}</span>
              </Button>
            </PopoverTrigger>
            <PopoverContent
              :class="isSidebarLayout
                ? 'w-[min(400px,calc(100vw-4.5rem))] p-0'
                : 'w-[min(400px,90vw)] p-0'"
              align="start"
              :side="isSidebarLayout ? 'right' : 'bottom'"
            >
              <PasteModal v-if="pasteOpen" @close="pasteOpen = false" />
            </PopoverContent>
          </Popover>
        </div>

        <!-- Device Recording - Adaptive -->
        <div v-if="showRecording && isVisible('record')">
          <Popover v-model:open="recordOpen">
            <PopoverTrigger as-child>
              <Button
                variant="ghost"
                size="sm"
                :class="['h-8 gap-1.5 text-xs', isRecording && 'text-red-600 dark:text-red-400']"
                :aria-label="t('recording.title')"
                :title="t('recording.title')"
              >
                <span v-if="isRecording" class="size-2.5 rounded-full bg-red-500" aria-hidden="true" />
                <CircleDot v-else class="size-4" aria-hidden="true" />
                <span data-action-label>{{ t('recording.short') }}</span>
              </Button>
            </PopoverTrigger>
            <PopoverContent
              :class="isSidebarLayout
                ? 'w-[min(420px,calc(100vw-4.5rem))] p-0'
                : 'w-[min(420px,92vw)] p-0'"
              align="start"
              :side="isSidebarLayout ? 'right' : 'bottom'"
              @interact-outside="keepOpenForDialogs"
            >
              <RecordingPanel v-if="recordOpen" :video-mode="props.videoMode" @switch-codec="emit('update:videoMode', $event)" />
            </PopoverContent>
          </Popover>
        </div>

      </ButtonGroup>

      <!-- Right side buttons -->
      <ButtonGroup
        class="shrink-0"
        :class="isSidebarLayout
          ? 'mt-auto flex-none flex-col'
          : 'ml-1 sm:ml-2'"
        :orientation="isSidebarLayout ? 'vertical' : 'horizontal'"
      >
        <VideoDisplayControls
          :is-fullscreen="props.isFullscreen"
          :minimal="minimalDisplayControls"
          :text-only-scale="isSidebarLayout"
          :scale-mode="props.scaleMode"
          :source-size-available="props.sourceSizeAvailable"
          @toggle-fullscreen="emit('toggleFullscreen')"
          @update:scale-mode="emit('update:scaleMode', $event)"
          @toggle-virtual-keyboard="emit('toggleVirtualKeyboard')"
        />

        <!-- Connection Stats - Adaptive -->
        <div v-if="isVisible('stats')">
          <TooltipProvider>
            <Tooltip>
              <TooltipTrigger as-child>
                <Button variant="ghost" size="sm" class="h-8 gap-1.5 text-xs" :aria-label="t('actionbar.stats')" :aria-pressed="!!props.statsOpen" :class="props.statsOpen && 'bg-accent text-accent-foreground'" @click="emit('toggleStats')">
                  <BarChart3 class="size-4" />
                  <span data-action-label>{{ t('actionbar.stats') }}</span>
                </Button>
              </TooltipTrigger>
              <TooltipContent>
                <p>{{ t('actionbar.statsTip') }}</p>
              </TooltipContent>
            </Tooltip>
          </TooltipProvider>
        </div>

        <!-- Web Terminal - Adaptive -->
        <div v-if="props.showTerminal !== false && isVisible('terminal')">
          <TooltipProvider>
            <Tooltip>
              <TooltipTrigger as-child>
                <Button
                  variant="ghost"
                  size="sm"
                  class="h-8 gap-1.5 text-xs"
                  :disabled="!props.ttydRunning"
                  :aria-label="t('actionbar.webTerminal')"
                  :aria-expanded="!!props.terminalOpen"
                  :aria-pressed="!!props.terminalOpen"
                  :class="props.terminalOpen && 'bg-accent text-accent-foreground'"
                  @click="emit('openTerminal')"
                >
                  <Terminal class="size-4" />
                  <span data-action-label>{{ t('actionbar.webTerminal') }}</span>
                </Button>
              </TooltipTrigger>
              <TooltipContent>
                <p>{{ t('actionbar.webTerminal') }}</p>
              </TooltipContent>
            </Tooltip>
          </TooltipProvider>
        </div>

        <!-- Computer Use - Optional -->
        <TooltipProvider v-if="isVisible('ai')">
          <Tooltip>
            <TooltipTrigger as-child>
              <Button
                variant="ghost"
                size="sm"
                class="h-8 gap-1.5 text-xs"
                :aria-label="t('computerUse.title')"
                @click="emit('openComputerUse')"
              >
                <Bot class="size-3.5 sm:size-4" />
                <span data-action-label>AI</span>
              </Button>
            </TooltipTrigger>
            <TooltipContent>
              <p>{{ t('computerUse.title') }}</p>
            </TooltipContent>
          </Tooltip>
        </TooltipProvider>

        <!-- Settings - Adaptive -->
        <div v-if="isVisible('settings')">
          <TooltipProvider>
            <Tooltip>
              <TooltipTrigger as-child>
                <Button variant="ghost" size="sm" class="h-8 gap-1.5 text-xs" :aria-label="t('actionbar.settings')" @click="router.push('/settings')">
                  <Settings class="size-4" />
                  <span data-action-label>{{ t('actionbar.settings') }}</span>
                </Button>
              </TooltipTrigger>
              <TooltipContent>
                <p>{{ t('actionbar.settingsTip') }}</p>
              </TooltipContent>
            </Tooltip>
          </TooltipProvider>
        </div>

        <!-- Overflow Menu - Only show if there are overflowed items -->
        <slot name="trailing" />
        <DropdownMenu v-if="!isSidebarLayout" v-model:open="overflowMenuOpen">
          <DropdownMenuTrigger as-child>
            <Button variant="ghost" size="sm" class="size-8 p-0" :aria-label="t('common.more')" :title="t('common.more')">
              <MoreHorizontal class="size-3.5 sm:size-4" />
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" :side="isSidebarLayout ? 'right' : 'bottom'" class="w-56 max-h-[70dvh] overflow-y-auto">
            <template v-if="minimalDisplayControls">
              <DropdownMenuItem @click="openFromOverflow(() => emit('toggleVirtualKeyboard'))">
                <Keyboard class="size-4 mr-2" />{{ t('actionbar.keyboard') }}
              </DropdownMenuItem>
              <DropdownMenuItem
                :disabled="!props.sourceSizeAvailable"
                @click="emit('update:scaleMode', props.scaleMode === 'actual' ? 'fit' : 'actual')"
              >
                <Scaling class="size-4 mr-2" />{{ t(props.scaleMode === 'actual' ? 'actionbar.fitSizeAria' : 'actionbar.actualSizeAria') }}
              </DropdownMenuItem>
              <DropdownMenuSeparator />
            </template>
            <!-- MSD -->
            <DropdownMenuItem v-if="showMsd && !isVisible('msd')" @click="openFromOverflow(() => msdDialogOpen = true)">
              <HardDrive class="size-4 mr-2" />
              {{ t('actionbar.virtualMedia') }}
            </DropdownMenuItem>

            <!-- ATX -->
            <DropdownMenuItem v-if="showAtx && !isVisible('atx')" @click="openMobileAtx">
              <Power class="size-4 mr-2" />
              {{ t('actionbar.power') }}
            </DropdownMenuItem>

            <!-- Paste -->
            <DropdownMenuItem v-if="showPasteText && !isVisible('paste')" @click="openMobilePaste">
              <ClipboardPaste class="size-4 mr-2" />
              {{ t('actionbar.paste') }}
            </DropdownMenuItem>

            <!-- Recording -->
            <DropdownMenuItem v-if="showRecording && !isVisible('record')" @click="openMobileRecord">
              <span v-if="isRecording" class="size-2.5 rounded-full bg-red-500" /><CircleDot v-else class="size-4" />
              {{ t('recording.title') }}
            </DropdownMenuItem>

            <DropdownMenuSeparator v-if="hasLeftOverflow && hasRightOverflow" />

            <!-- Stats -->
            <DropdownMenuItem v-if="showStats && !isVisible('stats')" @click="openFromOverflow(() => emit('toggleStats'))">
              <BarChart3 class="size-4 mr-2" />
              {{ t('actionbar.stats') }}
            </DropdownMenuItem>

            <!-- Web Terminal -->
            <DropdownMenuItem
              v-if="props.showTerminal !== false && !isVisible('terminal')"
              :aria-expanded="!!props.terminalOpen"
              :aria-pressed="!!props.terminalOpen"
              :class="props.terminalOpen && 'bg-accent text-accent-foreground'"
              :disabled="!props.ttydRunning"
              @click="openFromOverflow(() => emit('openTerminal'))"
            >
              <Terminal class="size-4 mr-2" />
              {{ t('actionbar.webTerminal') }}
            </DropdownMenuItem>

            <DropdownMenuItem v-if="props.showComputerUse !== false && !isVisible('ai')" @click="openFromOverflow(() => emit('openComputerUse'))">
              <Bot class="size-4 mr-2" />{{ t('computerUse.title') }}
            </DropdownMenuItem>

            <!-- Settings -->
            <DropdownMenuItem v-if="!isVisible('settings')" @click="openFromOverflow(() => router.push('/settings'))">
              <Settings class="size-4 mr-2" />
              {{ t('actionbar.settings') }}
            </DropdownMenuItem>
            <DropdownMenuSeparator v-if="!isSidebarLayout && (hasOverflow || minimalDisplayControls)" />
            <slot name="more" />
          </DropdownMenuContent>
        </DropdownMenu>
      </ButtonGroup>
    </div>
  </div>

  <!-- MSD Dialog -->
  <MsdDialog v-if="showMsd" v-model:open="msdDialogOpen" />

  <!-- Mobile ATX Sheet — used when ATX is opened from the overflow menu.
       A Sheet avoids the Popover anchor-positioning issues on mobile. -->
  <Sheet v-if="showAtx" v-model:open="mobileAtxOpen">
    <SheetContent
      side="bottom"
      class="max-h-[90dvh] overflow-y-auto"
      @pointer-down-outside="(e) => guardOutside(mobileAtxOpenTime, e)"
      @interact-outside="(e) => guardOutside(mobileAtxOpenTime, e)"
    >
      <SheetHeader class="mb-2">
        <SheetTitle>{{ t('actionbar.power') }}</SheetTitle>
      </SheetHeader>
      <AtxPopover
        :atx-enabled="atxEnabled"
        @close="mobileAtxOpen = false"
        @power-short="emit('powerShort')"
        @power-long="emit('powerLong')"
        @reset="emit('reset')"
        @wol="(mac) => emit('wol', mac)"
      />
    </SheetContent>
  </Sheet>

  <!-- Mobile Paste Sheet — used when Paste is opened from the overflow menu. -->
  <Sheet v-if="showPasteText" v-model:open="mobilePasteOpen">
    <SheetContent
      side="bottom"
      class="max-h-[90dvh] overflow-y-auto"
      @pointer-down-outside="(e) => guardOutside(mobilePasteOpenTime, e)"
      @interact-outside="(e) => guardOutside(mobilePasteOpenTime, e)"
    >
      <SheetHeader class="mb-2">
        <SheetTitle>{{ t('actionbar.paste') }}</SheetTitle>
      </SheetHeader>
      <PasteModal v-if="mobilePasteOpen" @close="mobilePasteOpen = false" />
    </SheetContent>
  </Sheet>

  <Sheet v-if="showRecording" v-model:open="mobileRecordOpen">
    <SheetContent
      side="bottom"
      class="max-h-[90dvh] overflow-y-auto p-0"
      @pointer-down-outside="(e) => guardOutside(mobileRecordOpenTime, e)"
      @interact-outside="(e) => { guardOutside(mobileRecordOpenTime, e); keepOpenForDialogs(e) }"
    >
      <SheetHeader class="sr-only">
        <SheetTitle>{{ t('recording.title') }}</SheetTitle>
      </SheetHeader>
      <RecordingPanel v-if="mobileRecordOpen" inline-location-picker :video-mode="props.videoMode" @switch-codec="emit('update:videoMode', $event)" />
    </SheetContent>
  </Sheet>

  <!-- Hidden measurement container: renders each collapsible button in both
       icon-only and with-label forms so we can read their real offsetWidth. -->
  <div ref="measureRef" aria-hidden="true" class="fixed pointer-events-none" style="visibility: hidden; top: -9999px; left: -9999px; white-space: nowrap;">
    <div class="flex items-center gap-0.5 sm:gap-1.5 px-2 sm:px-4 py-1 sm:py-1.5">
      <Button v-for="(label, id) in { video: t('actionbar.videoConfig'), audio: t('actionbar.audioConfig'), hid: t('actionbar.hidConfig'), fullscreen: t(props.isFullscreen ? 'actionbar.exitFullscreen' : 'actionbar.fullscreen'), keyboard: t('actionbar.keyboard') }" :key="id" :data-measure="id + '-label'" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><CircleDot class="size-4" />{{ label }}</Button>
      <!-- MSD -->
      <Button data-measure="msd-icon" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><HardDrive class="size-4" /></Button>
      <Button data-measure="msd-label" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><HardDrive class="size-4" />{{ t('actionbar.virtualMedia') }}</Button>
      <!-- ATX -->
      <Button data-measure="atx-icon" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><Power class="size-4" /></Button>
      <Button data-measure="atx-label" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><Power class="size-4" />{{ t('actionbar.power') }}</Button>
      <!-- Paste -->
      <Button data-measure="paste-icon" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><ClipboardPaste class="size-4" /></Button>
      <Button data-measure="paste-label" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><ClipboardPaste class="size-4" />{{ t('actionbar.paste') }}</Button>
      <Button data-measure="record-icon" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><i class="size-4" /></Button>
      <Button data-measure="record-label" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><i class="size-4" />{{ t('recording.short') }}</Button>
      <!-- Stats -->
      <Button data-measure="stats-icon" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><BarChart3 class="size-4" /></Button>
      <Button data-measure="stats-label" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><BarChart3 class="size-4" />{{ t('actionbar.stats') }}</Button>
      <!-- Web Terminal -->
      <Button data-measure="terminal-icon" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><Terminal class="size-4" /></Button>
      <Button data-measure="terminal-label" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><Terminal class="size-4" />{{ t('actionbar.webTerminal') }}</Button>
      <!-- Settings -->
      <Button data-measure="settings-icon" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><Settings class="size-4" /></Button>
      <Button data-measure="settings-label" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><Settings class="size-4" />{{ t('actionbar.settings') }}</Button>
      <Button data-measure="ai-icon" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><Bot class="size-4" /></Button>
      <Button data-measure="ai-label" variant="ghost" size="sm" class="h-8 gap-1.5 text-xs"><Bot class="size-4" />AI</Button>
    </div>
  </div>
</template>

<style scoped>
.console-action-bar { background: transparent; }
.console-action-bar__inner { gap: 2px; }
.console-action-bar:not(.console-action-bar--sidebar) .console-action-bar__inner :deep(button) { height: 32px; padding-inline: 10px; gap: 6px; }
.console-action-bar[data-label-mode='icons'] :deep([data-action-label]) { display: none; }
.console-action-bar:not(.console-action-bar--sidebar)[data-label-mode='icons'] .console-action-bar__inner :deep(button:not([aria-pressed])) { width: 32px; padding-inline: 8px; }
.console-action-bar:not(.console-action-bar--sidebar) .console-action-bar__inner :deep([data-console-divider]) { width: 1px; min-width: 1px; padding: 0; }
.console-action-bar--sidebar {
  position: absolute;
  z-index: 40;
  top: 2.5rem;
  bottom: 0;
  left: 0;
  width: 3.5rem;
  box-shadow: inset -1px 0 var(--border);
  background: color-mix(in srgb, var(--background) 94%, transparent);
  backdrop-filter: blur(12px);
}

.console-action-bar--sidebar :deep([data-slot='button-group']),
.console-action-bar--sidebar :deep([data-slot='button-group'] > :not([data-console-divider])) {
  width: 100%;
  min-width: 0;
}

.console-action-bar--sidebar :deep(button) {
  height: 36px;
  flex-shrink: 0;
  width: 100%;
  min-width: 0;
  gap: 0;
  justify-content: center;
  overflow: hidden;
  padding-inline: 0.5rem;
  border-radius: 0.5rem;
}

.console-action-bar--sidebar :deep([data-console-divider]) {
  width: 24px;
  min-width: 24px;
  height: 1px;
  padding: 0;
  margin: 6px auto;
}

.console-action-bar--sidebar .console-action-bar__inner {
  overflow-y: auto;
  overflow-x: hidden;
  gap: 1rem;
  /* A reserved scrollbar would shrink and offset the fixed-width icon column. */
  scrollbar-width: none;
}

@media (pointer: coarse) {
  .console-action-bar--sidebar :deep(button) { height: 44px; }
}
</style>
