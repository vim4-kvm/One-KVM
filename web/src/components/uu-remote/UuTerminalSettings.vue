<script setup lang="ts">
import { computed, nextTick, onBeforeUnmount, ref, watch } from 'vue'
import { useI18n } from 'vue-i18n'
import { uuRemoteApi } from '@/api'
import { ApiError } from '@/api/request'
import type { UuTerminalConfig, SshTestResponse } from '@/types/generated'
import { useClipboard } from '@/composables/useClipboard'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Label } from '@/components/ui/label'
import { Switch } from '@/components/ui/switch'
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from '@/components/ui/select'
import { Loader2 } from 'lucide-vue-next'
import { useUuRemoteContext } from './useUuRemote'

defineEmits<{ navigate: [section: 'hid'] }>()
const { t, locale } = useI18n()
const { response, busy, save, refresh, mutate } = useUuRemoteContext()
const config = computed(() => response.value!.config.terminal)
const status = computed(() => response.value!.status.terminal)
type Target = keyof UuTerminalConfig
const targets: Target[] = ['board', 'serial', 'ssh']
const baudRates = [9600, 19200, 38400, 57600, 115200, 230400, 460800, 921600, 1500000]
const pending = ref<Target | null>(null)
const errors = ref<Record<string, string>>({})
const saved = ref(false)
const host = ref(config.value.ssh.host)
const port = ref(String(config.value.ssh.port))
const user = ref(config.value.ssh.user)
const focused = ref(false)
const edited = ref(false)
const dirty = computed(
  () =>
    host.value !== config.value.ssh.host ||
    port.value !== String(config.value.ssh.port) ||
    user.value !== config.value.ssh.user
)
const testing = ref(false)
const testResult = ref<SshTestResponse | null>(null)
const copied = ref('')
const confirming = ref<number | null>(null)
const ending = ref<number | null>(null)
const { copy } = useClipboard()
let copyTimer: ReturnType<typeof setTimeout>
let savedTimer: ReturnType<typeof setTimeout>
let endTimer: ReturnType<typeof setTimeout>
const serialCommand = 'sudo systemctl enable --now serial-getty@ttyACM0'
const sessions = computed(() => [...(status.value?.sessions ?? [])].sort((a, b) => b.created_at_ms - a.created_at_ms))
const selectedDevice = computed(() => status.value?.serial.devices.find((d) => d.id === config.value.serial.device))
const busySession = computed(() => sessions.value.find((s) => s.id === status.value?.serial.busy_session))
const publicKey = computed(() => status.value?.ssh.public_key)
const shortPublicKey = computed(() => {
  const key = publicKey.value ?? ''
  return key.length > 64 ? `${key.slice(0, 28)}…${key.slice(-32)}` : key
})
const count = (target: Target) => sessions.value.filter((s) => s.target === target).length
const errorText = (error: unknown) => (error instanceof Error ? error.message : t('api.operationFailedDesc'))

watch(
  () => config.value.ssh,
  (next, previous) => {
    if (!focused.value && !edited.value) resetSsh()
    if (next.host !== previous.host || next.port !== previous.port || next.user !== previous.user)
      testResult.value = null
  }
)
watch([host, port, user], () => {
  testResult.value = null
  saved.value = false
})
function editSsh() {
  edited.value = true
  errors.value.ssh = ''
  testResult.value = null
}
function blurSsh() {
  focused.value = false
  void saveSsh()
}
function resetSsh() {
  host.value = config.value.ssh.host
  port.value = String(config.value.ssh.port)
  user.value = config.value.ssh.user
  edited.value = false
}
async function commit(target: Target, patch: Partial<UuTerminalConfig>) {
  errors.value[target] = ''
  try {
    // Compose from the latest saved object; unsaved SSH text never leaks into other rows.
    await save({ terminal: { ...config.value, ...patch } })
    return true
  } catch (error) {
    errors.value[target] = errorText(error)
    return false
  } finally {
    pending.value = null
  }
}
async function toggle(target: Target, enabled: boolean) {
  if (!enabled && count(target)) {
    pending.value = target
    return
  }
  await commit(target, { [target]: { ...config.value[target], enabled } })
}
async function disableTarget(target: Target) {
  await commit(target, { [target]: { ...config.value[target], enabled: false } })
}
async function saveSsh() {
  if (!edited.value) {
    resetSsh()
    return true
  }
  if (!dirty.value) {
    edited.value = false
    return true
  }
  if (busy.value || testing.value) return false
  if (
    [host.value, user.value].some((v) => v.startsWith('-') || /[\s\p{Cc}]/u.test(v)) ||
    [...host.value].length > 253 ||
    [...user.value].length > 64 ||
    !/^\d+$/.test(port.value) ||
    Number(port.value) < 1 ||
    Number(port.value) > 65535
  ) {
    errors.value.ssh = t('extensions.uuRemote.terminal.invalidSsh')
    return false
  }
  const ok = await commit('ssh', {
    ssh: { ...config.value.ssh, host: host.value, port: Number(port.value), user: user.value },
  })
  resetSsh()
  if (ok) {
    await nextTick()
    saved.value = true
    clearTimeout(savedTimer)
    savedTimer = setTimeout(() => {
      saved.value = false
    }, 1500)
  }
  return ok
}
async function testSsh() {
  if (!(await saveSsh()) || busy.value || testing.value) return
  testing.value = true
  errors.value.ssh = ''
  const tested = sshAddress()
  try {
    const result = await uuRemoteApi.testSsh()
    if (tested === sshAddress()) testResult.value = result
  } catch (error) {
    errors.value.ssh = errorText(error)
  } finally {
    testing.value = false
  }
}
function sshAddress() {
  const { host, port, user } = config.value.ssh
  return JSON.stringify([host, port, user])
}
async function trustHost() {
  const fingerprint = testResult.value?.fingerprint
  if (!fingerprint || dirty.value) return
  testing.value = true
  const tested = sshAddress()
  try {
    await uuRemoteApi.trustSsh(fingerprint)
    if (tested === sshAddress()) {
      const result = await uuRemoteApi.testSsh()
      if (tested === sshAddress()) testResult.value = result
    }
  } catch (error) {
    testResult.value = null
    errors.value.ssh =
      error instanceof ApiError && error.status === 409
        ? t('extensions.uuRemote.terminal.fingerprintChanged')
        : errorText(error)
  } finally {
    testing.value = false
  }
}
async function removeHost() {
  testing.value = true
  const tested = sshAddress()
  try {
    await uuRemoteApi.removeSshHost()
    if (tested === sshAddress()) {
      const result = await uuRemoteApi.testSsh()
      if (tested === sshAddress()) testResult.value = result
    }
  } catch (error) {
    errors.value.ssh = errorText(error)
  } finally {
    testing.value = false
  }
}
async function copyValue(kind: string, value: string) {
  if (await copy(value)) {
    copied.value = kind
    clearTimeout(copyTimer)
    copyTimer = setTimeout(() => {
      copied.value = ''
    }, 1500)
  } else errors.value[kind === 'key' ? 'ssh' : 'serial'] = t('extensions.uuRemote.terminal.copyFailed')
}
async function endSession(id: number) {
  if (confirming.value !== id) {
    confirming.value = id
    clearTimeout(endTimer)
    endTimer = setTimeout(() => {
      confirming.value = null
    }, 3000)
    return
  }
  clearTimeout(endTimer)
  ending.value = id
  errors.value.sessions = ''
  try {
    await mutate(async () => {
      await uuRemoteApi.endSession(id)
      return await uuRemoteApi.getStatus()
    })
  } catch (error) {
    errors.value.sessions = errorText(error)
    await refresh()
  } finally {
    ending.value = null
    confirming.value = null
  }
}
function relativeTime(timestamp: number) {
  const minutes = Math.max(0, Math.floor((Date.now() - timestamp) / 60000))
  const formatter = new Intl.RelativeTimeFormat(locale.value, { numeric: 'auto' })
  return minutes < 60
    ? formatter.format(-minutes, 'minute')
    : minutes < 1440
      ? formatter.format(-Math.floor(minutes / 60), 'hour')
      : formatter.format(-Math.floor(minutes / 1440), 'day')
}
const resultClass = computed(() =>
  testResult.value?.result === 'ok'
    ? 'text-emerald-600 dark:text-emerald-400'
    : ['password_required', 'unknown_host', 'not_configured'].includes(testResult.value?.result ?? '')
      ? 'text-amber-600 dark:text-amber-400'
      : 'text-destructive'
)
onBeforeUnmount(() => {
  clearTimeout(copyTimer)
  clearTimeout(savedTimer)
  clearTimeout(endTimer)
})
</script>

<template>
  <div class="min-w-0 space-y-4">
    <p class="text-xs text-muted-foreground">{{ t('extensions.uuRemote.terminal.intro') }}</p>
    <div class="divide-y rounded-lg border">
      <section v-for="target in targets" :key="target" class="min-w-0 space-y-3 p-4">
        <div class="flex items-center justify-between gap-3">
          <Label :for="`uu-terminal-${target}`">{{ t(`extensions.uuRemote.terminal.${target}`) }}</Label>
          <Switch
            :id="`uu-terminal-${target}`"
            :model-value="config[target].enabled"
            :disabled="busy || testing || !!pending"
            @update:model-value="toggle(target, $event)"
          />
        </div>
        <p class="text-xs text-muted-foreground">{{ t(`extensions.uuRemote.terminal.${target}Hint`) }}</p>
        <div v-if="pending === target" role="alert" class="space-y-2 rounded-md bg-amber-500/10 p-3 text-sm">
          <p>{{ t('extensions.uuRemote.terminal.closeWarning', { count: count(target) }) }}</p>
          <div class="flex flex-wrap gap-2">
            <Button size="sm" variant="destructive" :disabled="busy" @click="disableTarget(target)">
              {{ t('extensions.uuRemote.terminal.closeApply') }}
            </Button>
            <Button size="sm" variant="ghost" :disabled="busy" @click="pending = null">{{ t('common.cancel') }}</Button>
          </div>
        </div>
        <template v-if="config[target].enabled && target === 'serial'">
          <div class="grid min-w-0 gap-2 sm:grid-cols-[80px_minmax(0,1fr)] sm:items-center">
            <Label for="uu-serial-device">{{ t('extensions.uuRemote.terminal.device') }}</Label>
            <div class="min-w-0 space-y-2">
              <Select
                :model-value="config.serial.device || undefined"
                :disabled="busy"
                @update:model-value="(v) => commit('serial', { serial: { ...config.serial, device: String(v) } })"
              >
                <SelectTrigger id="uu-serial-device" class="w-full min-w-0 [&>span]:truncate">
                  <SelectValue :placeholder="t('extensions.uuRemote.terminal.selectDevice')" />
                </SelectTrigger>
                <SelectContent>
                  <SelectItem v-for="device in status?.serial.devices ?? []" :key="device.id" :value="device.id">
                    {{ device.label }}{{ device.present ? '' : ` (${t('extensions.uuRemote.terminal.disconnected')})` }}
                  </SelectItem>
                </SelectContent>
              </Select>
              <p
                class="text-xs"
                :class="
                  busySession
                    ? 'text-blue-600 dark:text-blue-400'
                    : selectedDevice?.present
                      ? 'text-emerald-600 dark:text-emerald-400'
                      : 'text-muted-foreground'
                "
              >
                {{
                  busySession
                    ? `● ${t('extensions.uuRemote.terminal.inUse', { name: busySession.name })}`
                    : selectedDevice?.present
                      ? `● ${t('extensions.uuRemote.terminal.ready')}`
                      : `○ ${t('extensions.uuRemote.terminal.disconnected')}`
                }}
              </p>
            </div>
            <Label for="uu-serial-baud">{{ t('extensions.uuRemote.terminal.baud') }}</Label>
            <div class="flex flex-wrap items-center gap-2">
              <Select
                :model-value="String(config.serial.baud)"
                :disabled="busy"
                @update:model-value="(v) => commit('serial', { serial: { ...config.serial, baud: Number(v) } })"
              >
                <SelectTrigger id="uu-serial-baud"><SelectValue /></SelectTrigger>
                <SelectContent>
                  <SelectItem v-for="baud in baudRates" :key="baud" :value="String(baud)">{{ baud }}</SelectItem>
                </SelectContent>
              </Select>
              <span class="text-xs text-muted-foreground">{{ t('extensions.uuRemote.terminal.serialMode') }}</span>
            </div>
          </div>
          <p v-if="!status?.serial.usb_gadget_enabled" class="text-xs text-muted-foreground">
            {{ t('extensions.uuRemote.terminal.gadgetHint') }}
            <Button variant="link" size="sm" class="h-auto p-0" @click="$emit('navigate', 'hid')">
              {{ t('extensions.uuRemote.terminal.openHid') }} ›
            </Button>
          </p>
          <div v-if="config.serial.device === 'usb-gadget'" class="space-y-2 text-xs text-muted-foreground">
            <p>{{ t('extensions.uuRemote.terminal.gettyHint') }}</p>
            <div class="flex min-w-0 items-center gap-2 rounded-md bg-muted p-2">
              <code class="min-w-0 flex-1 break-all select-text">{{ serialCommand }}</code>
              <Button size="sm" variant="ghost" @click="copyValue('command', serialCommand)">
                {{ t(copied === 'command' ? 'common.copied' : 'common.copy') }}
              </Button>
            </div>
            <p v-if="response?.config.system === 'macos'">{{ t('extensions.uuRemote.terminal.macSerial') }}</p>
          </div>
        </template>
        <template v-if="config[target].enabled && target === 'ssh'">
          <div
            class="grid min-w-0 grid-cols-[minmax(0,1fr)_80px] gap-2 sm:grid-cols-[minmax(0,2fr)_80px_minmax(0,1fr)]"
            @input="editSsh"
          >
            <div class="space-y-1">
              <Label for="uu-ssh-host">{{ t('extensions.uuRemote.terminal.host') }}</Label>
              <Input
                id="uu-ssh-host"
                v-model="host"
                :disabled="busy || testing"
                :aria-invalid="!!errors.ssh"
                @focus="focused = true"
                @blur="blurSsh"
                @keydown.enter.prevent="saveSsh"
                @keydown.esc="resetSsh"
              />
            </div>
            <div class="space-y-1">
              <Label for="uu-ssh-port">{{ t('extensions.uuRemote.terminal.port') }}</Label>
              <Input
                id="uu-ssh-port"
                v-model="port"
                type="number"
                min="1"
                max="65535"
                :disabled="busy || testing"
                @focus="focused = true"
                @blur="blurSsh"
                @keydown.enter.prevent="saveSsh"
                @keydown.esc="resetSsh"
              />
            </div>
            <div class="col-span-2 space-y-1 sm:col-span-1">
              <Label for="uu-ssh-user">{{ t('extensions.uuRemote.terminal.user') }}</Label>
              <Input
                id="uu-ssh-user"
                v-model="user"
                :disabled="busy || testing"
                @focus="focused = true"
                @blur="blurSsh"
                @keydown.enter.prevent="saveSsh"
                @keydown.esc="resetSsh"
              />
            </div>
          </div>
          <span v-if="saved" role="status" class="text-emerald-600" :aria-label="t('common.saved')">✓</span>
          <div class="space-y-1">
            <Label>{{ t('extensions.uuRemote.terminal.publicKey') }}</Label>
            <div class="flex min-w-0 items-center gap-2">
              <code v-if="publicKey" class="min-w-0 flex-1 break-all text-xs select-text">{{ shortPublicKey }}</code>
              <span v-else class="text-xs text-muted-foreground">
                {{ t('extensions.uuRemote.terminal.keyPending') }}
              </span>
              <Button v-if="publicKey" variant="ghost" size="sm" @click="copyValue('key', publicKey)">
                {{ t(copied === 'key' ? 'common.copied' : 'common.copy') }}
              </Button>
            </div>
            <p class="text-xs text-muted-foreground">{{ t('extensions.uuRemote.terminal.keyHint') }}</p>
          </div>
          <Button variant="outline" size="sm" :disabled="!host || busy || testing" @click="testSsh">
            <Loader2 v-if="testing" class="size-4 animate-spin" />
            {{ t('extensions.uuRemote.terminal.test') }}
          </Button>
          <div v-if="testResult" role="status" class="space-y-1 break-words text-xs" :class="resultClass">
            <p>
              {{
                t(`extensions.uuRemote.terminal.results.${testResult.result}`, { message: testResult.message ?? '' })
              }}
              <code v-if="testResult.result === 'unknown_host'" class="break-all select-text">
                {{ testResult.fingerprint }}
              </code>
            </p>
            <Button
              v-if="testResult.result === 'unknown_host' && testResult.fingerprint"
              variant="outline"
              size="sm"
              :disabled="testing || busy || dirty"
              @click="trustHost"
            >
              {{ t('extensions.uuRemote.terminal.trust') }}
            </Button>
            <Button
              v-if="testResult.result === 'host_key_changed'"
              variant="outline"
              size="sm"
              :disabled="testing || busy || dirty"
              @click="removeHost"
            >
              {{ t('extensions.uuRemote.terminal.removeKey') }}
            </Button>
          </div>
        </template>
        <p v-if="errors[target]" class="break-words text-xs text-destructive" role="alert">{{ errors[target] }}</p>
      </section>
    </div>
    <h4 class="text-sm font-medium">{{ t('extensions.uuRemote.terminal.sessions') }}</h4>
    <p v-if="!sessions.length" class="text-sm text-muted-foreground">
      {{ t('extensions.uuRemote.terminal.noSessions') }}
    </p>
    <div
      v-for="session in sessions"
      :key="session.id"
      class="grid min-w-0 grid-cols-[minmax(0,1fr)_auto] items-center gap-2 border-b pb-3 text-xs"
    >
      <div class="min-w-0">
        <strong class="break-words text-sm">{{ session.name }}</strong>
        <span class="ml-2 break-words text-muted-foreground">
          {{ t(`extensions.uuRemote.terminal.${session.target}`) }} · {{ session.detail }}
        </span>
        <div class="mt-1 flex flex-wrap gap-x-3 text-muted-foreground">
          <span>
            {{
              t(session.attached ? 'extensions.uuRemote.terminal.attached' : 'extensions.uuRemote.terminal.detached')
            }}
          </span>
          <span>{{ relativeTime(session.created_at_ms) }}</span>
        </div>
      </div>
      <Button
        size="sm"
        :variant="confirming === session.id ? 'destructive' : 'ghost'"
        :disabled="ending !== null || busy"
        @click="endSession(session.id)"
      >
        {{
          t(confirming === session.id ? 'extensions.uuRemote.terminal.confirmEnd' : 'extensions.uuRemote.terminal.end')
        }}
      </Button>
    </div>
    <p v-if="errors.sessions" class="text-xs text-destructive" role="alert">{{ errors.sessions }}</p>
  </div>
</template>
