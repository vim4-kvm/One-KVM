<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import type { UuVideoStats } from '@/api/config'

defineProps<{ stats?: UuVideoStats | null; stale: boolean }>()
const { t } = useI18n()
const label = (key: string) => t(`extensions.uuRemote.metrics.${key}`)
const value = (n: number | null | undefined, unit = 'ms') =>
  n == null || !Number.isFinite(n) ? label('unavailable') : `${n.toFixed(2)} ${unit}`
const age = (ms: number) => t('extensions.uuRemote.metrics.age', { seconds: Math.floor(ms / 1000) })
</script>

<template>
  <div class="space-y-3 px-4 pt-3" data-testid="uu-video-stats">
    <div class="grid gap-3 sm:grid-cols-2">
      <section class="space-y-2 rounded-md bg-muted/30 p-3">
        <h4 class="text-xs font-medium">{{ label('receiver') }}</h4>
        <template v-if="stats?.receiver">
          <p :class="['text-xs', stale || stats.receiver.age_ms > 10000 ? 'text-amber-600' : 'text-muted-foreground']">
            {{ age(stats.receiver.age_ms) }}<span v-if="stale || stats.receiver.age_ms > 10000"> · {{ label('stale') }}</span>
          </p>
          <dl class="grid grid-cols-[1fr_auto] gap-x-3 gap-y-1 text-xs tabular-nums">
            <dt>{{ label('rtt') }}</dt><dd>{{ value(stats.receiver.rtt_ms) }}</dd>
            <dt>{{ label('loss') }}</dt><dd>{{ value(stats.receiver.loss_percent, '%') }}</dd>
            <dt>{{ label('lost') }}</dt><dd>{{ stats.receiver.total_lost }}</dd>
            <dt>{{ label('jitter') }}</dt><dd>{{ value(stats.receiver.jitter_ms) }}</dd>
          </dl>
        </template>
        <p v-else class="text-xs text-muted-foreground">{{ label('unavailable') }}</p>
        <p v-if="stats" class="text-xs text-muted-foreground">
          {{ label('requests') }}: {{ stats.nack_packet_requests }} / {{ stats.pli }} / {{ stats.fir }}
        </p>
      </section>
      <section class="space-y-2 rounded-md bg-muted/30 p-3">
        <h4 class="text-xs font-medium">{{ label('sender') }}</h4>
        <template v-if="stats?.sender">
          <p :class="['text-xs', stale || stats.sender.age_ms > 10000 ? 'text-amber-600' : 'text-muted-foreground']">
            {{ age(stats.sender.age_ms) }}<span v-if="stale || stats.sender.age_ms > 10000"> · {{ label('stale') }}</span>
          </p>
          <dl class="grid grid-cols-[1fr_auto] gap-x-3 gap-y-1 text-xs tabular-nums">
            <dt>{{ label('fps') }}</dt><dd>{{ value(stats.sender.fps, 'fps') }}</dd>
            <dt>{{ label('meanQueue') }}</dt><dd>{{ value(stats.sender.mean_queue_ms) }}</dd>
            <dt>{{ label('maxQueue') }}</dt><dd>{{ value(stats.sender.max_queue_ms) }}</dd>
            <dt>{{ label('maxAge') }}</dt><dd>{{ value(stats.sender.max_encoded_to_send_ms) }}</dd>
          </dl>
          <p class="text-xs text-muted-foreground">{{ t('extensions.uuRemote.metrics.window', { seconds: (stats.sender.window_ms / 1000).toFixed(1), cached: stats.sender.cached_frames }) }}</p>
        </template>
        <p v-else class="text-xs text-muted-foreground">{{ label('unavailable') }}</p>
      </section>
    </div>
    <p class="text-xs leading-relaxed text-muted-foreground">{{ label('hint') }}</p>
  </div>
</template>
