<script setup lang="ts">
import { onMounted, onUnmounted } from 'vue'
import { useI18n } from 'vue-i18n'
import { Button } from '@/components/ui/button'
import {
  AlertDialog, AlertDialogCancel, AlertDialogContent, AlertDialogDescription,
  AlertDialogFooter, AlertDialogHeader, AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import { formatClock, useDeviceRecording } from '@/composables/useDeviceRecording'

const { t } = useI18n()
const recording = useDeviceRecording()
const { status, elapsedSeconds, codecSwitchPrompt } = recording

onMounted(recording.attach)
onUnmounted(recording.detach)
</script>

<template>
  <button
    data-recording-indicator
    v-if="status?.recording"
    type="button"
    :title="t('recording.indicator')"
    :aria-label="t('recording.indicator')"
    class="inline-flex h-7 shrink-0 items-center gap-1.5 rounded-full bg-red-500/10 px-2.5 text-xs font-medium text-red-600 ring-1 ring-inset ring-red-500/30 transition-colors hover:bg-red-500/15 focus-visible:outline-none focus-visible:ring-2 focus-visible:ring-ring dark:text-red-400"
    @click="recording.requestOpen()"
  >
    <span class="relative flex size-2">
      <span class="absolute inline-flex size-full animate-ping rounded-full bg-red-500/60" />
      <span class="relative inline-flex size-2 rounded-full bg-red-500" />
    </span>
    <span class="hidden sm:inline">REC</span>
    <span class="font-mono tabular-nums">{{ formatClock(elapsedSeconds, true) }}</span>
  </button>

  <AlertDialog :open="!!codecSwitchPrompt" @update:open="value => { if (!value) codecSwitchPrompt?.(false) }">
    <AlertDialogContent>
      <AlertDialogHeader>
        <AlertDialogTitle>{{ t('recording.codecSwitchTitle') }}</AlertDialogTitle>
        <AlertDialogDescription>{{ t('recording.codecSwitchBody') }}</AlertDialogDescription>
      </AlertDialogHeader>
      <AlertDialogFooter>
        <AlertDialogCancel>{{ t('recording.keepRecording') }}</AlertDialogCancel>
        <Button variant="destructive" @click="codecSwitchPrompt?.(true)">{{ t('recording.switchAnyway') }}</Button>
      </AlertDialogFooter>
    </AlertDialogContent>
  </AlertDialog>
</template>
