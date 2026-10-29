import { computed } from 'vue'
import type { ConsoleStatusItem } from '@/components/ConsoleStatusSummary.vue'

// Both the collapsed pill and the detail trigger use the same issue policy.
export function useConsoleStatus(items: () => ConsoleStatusItem[], t: (key: string) => string) {
  const issues = computed(() => items().filter(item =>
    ['error', 'no_signal', 'busy'].includes(item.status) || (item.required && item.status !== 'connected'),
  ))
  const summary = computed(() => issues.value.length
    ? issues.value.map(item => `${item.title}: ${t(`status.${item.status}`)}`).join(' · ')
    : t('status.connected'))
  const videoInfo = computed(() => items().find(item => item.id === 'video')?.quickInfo)
  const hasError = computed(() => issues.value.some(item => item.status === 'error'))
  return { issues, summary, videoInfo, hasError }
}
