<script setup lang="ts">
import { computed, onBeforeUnmount, onMounted, provide, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import QrcodeVue from 'qrcode.vue'
import { Loader2, MoreHorizontal, UserRound } from 'lucide-vue-next'
import { uuRemoteApi, type UuLoginState } from '@/api'
import { Card, CardContent, CardDescription, CardHeader, CardTitle } from '@/components/ui/card'
import { Button } from '@/components/ui/button'
import { Switch } from '@/components/ui/switch'
import { Tabs, TabsContent, TabsList, TabsTrigger } from '@/components/ui/tabs'
import { Badge } from '@/components/ui/badge'
import { DropdownMenu, DropdownMenuContent, DropdownMenuItem, DropdownMenuTrigger } from '@/components/ui/dropdown-menu'
import {
  AlertDialog,
  AlertDialogAction,
  AlertDialogCancel,
  AlertDialogContent,
  AlertDialogDescription,
  AlertDialogFooter,
  AlertDialogHeader,
  AlertDialogTitle,
} from '@/components/ui/alert-dialog'
import UuConnections from './UuConnections.vue'
import UuDesktopSettings from './UuDesktopSettings.vue'
import UuTerminalSettings from './UuTerminalSettings.vue'
import { uuRemoteKey, type UuRemoteController } from './useUuRemote'

const props = defineProps<{ remote: UuRemoteController; active: boolean; isVim4: boolean }>()
defineEmits<{ navigate: [section: 'video' | 'hid'] }>()
provide(uuRemoteKey, props.remote)
const { response, busy, stale, save, mutate, refresh, runLoginAction } = props.remote
const { t } = useI18n()
const tab = ref('connections')
try {
  const stored = localStorage.getItem('uu-remote-tab')
  if (stored && ['connections', 'desktop', 'terminal'].includes(stored)) tab.value = stored
} catch {
  /* Private browsing can disable storage. */
}
watch(tab, (value) => {
  try {
    localStorage.setItem('uu-remote-tab', value)
  } catch {
    /* Optional preference. */
  }
})
const tabs = ['connections', 'desktop', 'terminal'] as const
const connectionCount = computed(
  () =>
    (response.value?.status.sessions?.length ?? response.value?.status.connection_count ?? 0) +
    (response.value?.status.terminal?.peers ?? 0)
)
const serviceState = computed(() => response.value?.status.state ?? 'stopped')
const stateClass = computed(
  () =>
    ({
      stopped: 'bg-muted-foreground',
      connecting: 'animate-pulse bg-muted-foreground',
      waiting_login: 'bg-amber-500',
      online: 'bg-emerald-500',
      error: 'bg-destructive',
    })[serviceState.value]
)
const error = ref('')
const login = ref<UuLoginState>({ stage: 'idle' })
const loginBusy = ref(false)
const relogin = ref(false)
const unbinding = ref(false)
const visible = ref(!document.hidden)
const showLogin = computed(
  () => !!response.value?.config.enabled && (!response.value.status.logged_in || relogin.value)
)
const loginAllowed = computed(() => props.active && visible.value && showLogin.value)
let loginTimer: ReturnType<typeof setTimeout> | undefined
let loginRevision = 0
let disposed = false
function message(cause: unknown) {
  return cause instanceof Error ? cause.message : t('api.operationFailedDesc')
}

function queueLogin(action: () => Promise<void>) {
  return runLoginAction(action).catch((cause) => {
    if (!disposed) error.value = message(cause)
  })
}
function cancelLogin() {
  loginRevision++
  clearTimeout(loginTimer)
  loginBusy.value = false
  login.value = { stage: 'idle' }
  return queueLogin(async () => {
    await uuRemoteApi.cancelLogin()
  })
}
function startLogin() {
  const revision = ++loginRevision
  clearTimeout(loginTimer)
  loginBusy.value = true
  error.value = ''
  return queueLogin(async () => {
    try {
      if (!loginAllowed.value || disposed || revision !== loginRevision) return
      const next = await uuRemoteApi.startLogin()
      if (revision !== loginRevision || disposed) return
      login.value = next
      scheduleLoginPoll(revision)
    } catch (cause) {
      if (revision === loginRevision) {
        login.value = { stage: 'failed' }
        error.value = message(cause)
      }
    } finally {
      if (revision === loginRevision) loginBusy.value = false
    }
  })
}
function scheduleLoginPoll(revision: number) {
  if (!['waiting', 'scanned'].includes(login.value.stage)) return
  loginTimer = setTimeout(async () => {
    if (!loginAllowed.value || revision !== loginRevision || disposed) return
    try {
      const next = await uuRemoteApi.getLogin()
      if (revision !== loginRevision || disposed) return
      login.value = next
      if (next.stage === 'success') {
        relogin.value = false
        await refresh()
      } else scheduleLoginPoll(revision)
    } catch (cause) {
      if (revision === loginRevision) {
        login.value = { stage: 'failed' }
        error.value = message(cause)
      }
    }
  }, 2000)
}
watch(
  [loginAllowed, () => response.value?.config.system],
  ([allowed], previous) => {
    if (previous?.[0]) void cancelLogin()
    if (allowed) void startLogin()
  },
  { immediate: true }
)
async function toggle(enabled: boolean) {
  error.value = ''
  try {
    await save({ enabled })
  } catch (cause) {
    error.value = message(cause)
  }
}
async function logout() {
  error.value = ''
  try {
    await cancelLogin()
    await mutate(uuRemoteApi.logout)
    relogin.value = false
    unbinding.value = false
  } catch (cause) {
    error.value = message(cause)
  }
}
function visibilityChanged() {
  visible.value = !document.hidden
}
onMounted(() => document.addEventListener('visibilitychange', visibilityChanged))
onBeforeUnmount(() => {
  disposed = true
  document.removeEventListener('visibilitychange', visibilityChanged)
  if (showLogin.value) void cancelLogin()
})
</script>

<template>
  <Card class="min-w-0">
    <CardHeader>
      <div class="grid grid-cols-[minmax(0,1fr)_auto] items-start gap-3">
        <div class="flex min-w-0 flex-col gap-2 sm:flex-row sm:items-center sm:justify-between">
          <CardTitle>{{ t('extensions.uuRemote.title') }}</CardTitle>
          <span class="flex flex-wrap items-center gap-2 text-sm" role="status">
            <span class="size-2 rounded-full" :class="stateClass" />
            {{ t(`extensions.uuRemote.states.${serviceState}`) }}
            <span v-if="connectionCount">· {{ t('extensions.uuRemote.clients', { count: connectionCount }) }}</span>
          </span>
        </div>
        <div class="flex items-center gap-2">
          <Loader2 v-if="busy" class="size-4 animate-spin" />
          <Switch
            :model-value="response?.config.enabled ?? false"
            :disabled="busy || !response"
            :aria-label="t('extensions.uuRemote.title')"
            @update:model-value="toggle"
          />
        </div>
      </div>
      <CardDescription>{{ t('extensions.uuRemote.desc') }}</CardDescription>
      <p v-if="error || response?.status.error" role="alert" class="break-words text-sm text-destructive">
        {{ error || response?.status.error }}
      </p>
    </CardHeader>
    <CardContent class="min-w-0 space-y-4">
      <template v-if="response">
        <p v-if="!response.config.enabled" class="text-sm text-muted-foreground">
          {{ t('extensions.uuRemote.disabledHint') }}
        </p>
        <div
          v-else-if="response.status.logged_in && !relogin"
          class="flex min-w-0 items-center gap-2 rounded-lg border px-3 py-2 text-sm"
        >
          <UserRound class="size-4 shrink-0 text-muted-foreground" />
          <span class="min-w-0 flex-1 break-words">
            {{ response.status.nickname || response.status.device_id }} ·
            {{ response.config.system === 'macos' ? 'Mac' : 'Windows' }} · {{ response.config.device_name }}
          </span>
          <DropdownMenu>
            <DropdownMenuTrigger as-child>
              <Button
                variant="ghost"
                size="icon-sm"
                :disabled="busy"
                :aria-label="t('extensions.uuRemote.accountActions')"
              >
                <MoreHorizontal class="size-4" />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end">
              <DropdownMenuItem @select="relogin = true">{{ t('extensions.uuRemote.relogin') }}</DropdownMenuItem>
              <DropdownMenuItem class="text-destructive" @select="unbinding = true">
                {{ t('extensions.uuRemote.logout') }}
              </DropdownMenuItem>
            </DropdownMenuContent>
          </DropdownMenu>
        </div>
        <div
          v-if="showLogin"
          class="relative flex min-w-0 flex-col items-center gap-4 rounded-lg border p-4 sm:flex-row"
        >
          <Button
            v-if="relogin"
            variant="ghost"
            size="sm"
            class="self-end sm:absolute sm:right-2 sm:top-2"
            @click="relogin = false"
          >
            {{ t('common.cancel') }}
          </Button>
          <div
            class="relative flex size-[184px] shrink-0 flex-col items-center justify-center gap-2 rounded-md bg-white p-2 text-gray-900"
          >
            <QrcodeVue
              v-if="login.qr_url && ['waiting', 'scanned'].includes(login.stage)"
              :value="login.qr_url"
              :size="168"
              level="M"
            />
            <div
              v-if="login.stage === 'scanned'"
              class="absolute inset-0 flex items-center justify-center bg-white/90 p-4 text-center text-sm"
            >
              {{ t('extensions.uuRemote.scanned') }}
            </div>
            <Loader2 v-else-if="loginBusy || login.stage === 'idle'" class="size-5 animate-spin" />
            <template v-else-if="login.stage === 'failed'">
              <span class="text-sm">{{ t('extensions.uuRemote.qrExpired') }}</span>
              <Button size="sm" variant="secondary" @click="startLogin">{{ t('common.refresh') }}</Button>
            </template>
          </div>
          <div class="min-w-0 flex-1 space-y-3 sm:pt-5">
            <p class="text-sm font-medium">{{ t('extensions.uuRemote.scanTitle') }}</p>
            <p class="text-xs text-muted-foreground">{{ t('extensions.uuRemote.scanHint') }}</p>
            <UuDesktopSettings identity-only :is-vim4="isVim4" />
          </div>
        </div>
        <Tabs v-model="tab" class="min-w-0">
          <TabsList class="w-full">
            <TabsTrigger v-for="item in tabs" :key="item" :value="item" class="min-w-0 flex-1 gap-1">
              {{ t(`extensions.uuRemote.tabs.${item}`) }}
              <Badge v-if="item === 'connections' && connectionCount" variant="secondary" class="px-1.5">
                {{ connectionCount }}
              </Badge>
              <Badge
                v-if="item === 'terminal' && response.status.terminal?.sessions.length"
                variant="secondary"
                class="px-1.5"
              >
                {{ response.status.terminal.sessions.length }}
              </Badge>
            </TabsTrigger>
          </TabsList>
          <p v-if="stale" class="mt-3 text-xs text-amber-600 dark:text-amber-400" role="status">
            {{ t('extensions.uuRemote.statusStale') }}
          </p>
          <TabsContent value="connections" class="min-w-0 pt-3">
            <UuConnections @terminal="tab = 'terminal'" />
          </TabsContent>
          <TabsContent value="desktop" class="min-w-0 pt-3">
            <UuDesktopSettings :is-vim4="isVim4" @navigate="$emit('navigate', $event)" />
          </TabsContent>
          <TabsContent value="terminal" class="min-w-0 pt-3">
            <UuTerminalSettings
              v-if="response.config.terminal && response.status.terminal"
              @navigate="$emit('navigate', $event)"
            />
            <p v-else class="text-sm text-muted-foreground">
              {{ t('extensions.uuRemote.terminal.unavailable') }}
            </p>
          </TabsContent>
        </Tabs>
      </template>
      <p v-else class="text-sm text-muted-foreground" role="status">
        {{ t(stale ? 'extensions.uuRemote.statusStale' : 'common.loading') }}
      </p>
    </CardContent>
    <AlertDialog v-model:open="unbinding">
      <AlertDialogContent>
        <AlertDialogHeader>
          <AlertDialogTitle>{{ t('extensions.uuRemote.unbindTitle') }}</AlertDialogTitle>
          <AlertDialogDescription>
            {{ t('extensions.uuRemote.unbindBody', { name: response?.config.device_name }) }}
          </AlertDialogDescription>
        </AlertDialogHeader>
        <AlertDialogFooter>
          <AlertDialogCancel :disabled="busy">{{ t('common.cancel') }}</AlertDialogCancel>
          <AlertDialogAction
            class="bg-destructive text-white hover:bg-destructive/90"
            :disabled="busy"
            @click.prevent="logout"
          >
            {{ t('extensions.uuRemote.logout') }}
          </AlertDialogAction>
        </AlertDialogFooter>
      </AlertDialogContent>
    </AlertDialog>
  </Card>
</template>
