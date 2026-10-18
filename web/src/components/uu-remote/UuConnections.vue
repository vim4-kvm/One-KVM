<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import { Button } from '@/components/ui/button'
import UuVideoStats from '@/components/UuVideoStats.vue'
import { useUuRemoteContext } from './useUuRemote'
defineEmits<{ terminal: [] }>()
const { response, stale } = useUuRemoteContext()
const { t, te } = useI18n()
function stateLabel(state: string) {
  const key = `extensions.uuRemote.connectionStates.${state}`
  return te(key) ? t(key) : state
}
</script>

<template>
  <div class="space-y-3">
    <p
      v-if="!response?.status.sessions?.length && !response?.status.terminal?.peers"
      class="text-sm text-muted-foreground"
    >
      {{ t('extensions.uuRemote.noConnections', { name: response?.config.device_name }) }}
    </p>
    <div v-for="session in response?.status.sessions ?? []" :key="session.id" class="min-w-0 rounded-lg border text-sm">
      <div class="flex flex-wrap items-center justify-between gap-x-4 gap-y-2 px-4 pt-3">
        <div class="flex flex-wrap items-center gap-2">
          <strong>{{ t('extensions.uuRemote.tabs.desktop') }}</strong>
          <span
            class="size-2 rounded-full"
            :class="stale ? 'bg-muted-foreground' : session.state === 'connected' ? 'bg-emerald-500' : 'bg-amber-500'"
          />
          <span>{{ stateLabel(session.state) }}</span>
          <span v-if="session.link" class="text-xs text-muted-foreground">
            {{ t(session.link.relayed ? 'extensions.uuRemote.relayed' : 'extensions.uuRemote.direct') }} ·
            {{ session.link.local.protocol.toUpperCase() }} {{ session.link.local.address_family ?? 'IP' }}
          </span>
          <span class="text-xs text-muted-foreground">
            {{
              session.video_codec === 'h265' ? 'H.265' : session.video_codec === 'h264' ? 'H.264' : session.video_codec
            }}
          </span>
        </div>
        <span
          v-if="session.state === 'connected' && session.video_stats?.receiver"
          class="text-xs text-muted-foreground"
        >
          <template v-if="session.video_stats.receiver.rtt_ms !== null">
            RTT {{ session.video_stats.receiver.rtt_ms.toFixed(0) }} ms ·
          </template>
          {{ t('extensions.uuRemote.loss', { percent: session.video_stats.receiver.loss_percent.toFixed(1) }) }}
        </span>
      </div>
      <details class="min-w-0">
        <summary class="cursor-pointer px-4 py-3 text-xs text-muted-foreground">
          {{ t('extensions.uuRemote.statsDetails') }}
        </summary>
        <UuVideoStats :stats="session.video_stats" :stale="stale" />
        <div class="space-y-3 border-t p-4">
          <h5 class="text-xs font-medium">{{ t('extensions.uuRemote.connectionDetails') }}</h5>
          <dl class="grid grid-cols-[auto_minmax(0,1fr)] gap-x-4 gap-y-2 text-xs">
            <dt class="text-muted-foreground">{{ t('extensions.uuRemote.sessionId') }}</dt>
            <dd class="break-all font-mono select-text">{{ session.id }}</dd>
            <template v-if="session.client_id">
              <dt class="text-muted-foreground">{{ t('extensions.uuRemote.clientId') }}</dt>
              <dd class="break-all font-mono select-text">{{ session.client_id }}</dd>
            </template>
            <dt class="text-muted-foreground">ICE</dt>
            <dd>{{ stateLabel(session.ice_state) }}</dd>
          </dl>
          <div v-if="session.link" class="grid min-w-0 gap-2 sm:grid-cols-2">
            <div
              v-for="side in ['local', 'remote'] as const"
              :key="side"
              class="min-w-0 space-y-2 rounded-md bg-muted/40 p-3 text-xs"
            >
              <div class="flex flex-wrap justify-between gap-2">
                <strong>{{ t(`extensions.uuRemote.${side}`) }}</strong>
                <span>{{ session.link[side].candidate_type }}</span>
              </div>
              <p class="break-all font-mono select-text">
                {{
                  session.link[side].address.includes(':')
                    ? `[${session.link[side].address}]`
                    : session.link[side].address
                }}:{{ session.link[side].port }}
              </p>
              <p>{{ session.link[side].protocol.toUpperCase() }} · {{ session.link[side].address_family ?? 'IP' }}</p>
            </div>
          </div>
          <p v-else class="text-xs text-muted-foreground">{{ t('extensions.uuRemote.noLink') }}</p>
          <p class="text-xs text-muted-foreground">{{ t('extensions.uuRemote.linkHint') }}</p>
        </div>
      </details>
    </div>
    <div
      v-if="response?.status.terminal?.peers"
      class="flex flex-wrap items-center justify-between gap-2 rounded-lg border p-3 text-sm"
    >
      <span>
        <strong>{{ t('extensions.uuRemote.tabs.terminal') }}</strong>
        <span class="text-emerald-600">●</span>
        {{
          t('extensions.uuRemote.terminalConnections', {
            peers: response.status.terminal.peers,
            sessions: response.status.terminal.sessions.length,
          })
        }}
      </span>
      <Button size="sm" variant="ghost" @click="$emit('terminal')">
        {{ t('extensions.uuRemote.viewSessions') }} ›
      </Button>
    </div>
  </div>
</template>
