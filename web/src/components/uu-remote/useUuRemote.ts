import { inject, ref, type InjectionKey } from 'vue'
import { uuRemoteApi, type UuRemoteStatusResponse } from '@/api'
import i18n from '@/i18n'

export function useUuRemote() {
  const response = ref<UuRemoteStatusResponse | null>(null)
  const busy = ref(false)
  const stale = ref(false)
  let revision = 0
  let polling = false
  let loginQueue: Promise<void> = Promise.resolve()

  // Keep login ordering across card unmount/remount, including a pending POST
  // followed by cancellation when the user leaves the settings section.
  function runLoginAction(action: () => Promise<void>) {
    const result = loginQueue.then(action)
    loginQueue = result.catch(() => undefined)
    return result
  }

  async function refresh() {
    if (busy.value || polling) return
    polling = true
    const started = revision
    try {
      const next = await uuRemoteApi.getStatus()
      // A GET started before a mutation must not restore the old configuration.
      if (started === revision && !busy.value) {
        response.value = next
        stale.value = false
      }
    } catch {
      if (started === revision) stale.value = true
    } finally {
      polling = false
    }
  }

  async function mutate(action: () => Promise<UuRemoteStatusResponse>) {
    if (busy.value) throw new Error(String(i18n.global.t('extensions.uuRemote.busy')))
    busy.value = true
    revision++
    try {
      response.value = await action()
      stale.value = false
    } finally {
      busy.value = false
    }
  }

  const save = (patch: Partial<UuRemoteStatusResponse['config']>) => mutate(() => uuRemoteApi.update(patch))
  return { response, busy, stale, refresh, save, mutate, runLoginAction }
}

export type UuRemoteController = ReturnType<typeof useUuRemote>
export const uuRemoteKey: InjectionKey<UuRemoteController> = Symbol('uu-remote')
export function useUuRemoteContext() {
  const controller = inject(uuRemoteKey)
  if (!controller) throw new Error('UU Remote settings require UuRemoteCard.')
  return controller
}
