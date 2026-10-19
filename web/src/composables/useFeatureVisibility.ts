import { useLocalStorage } from '@vueuse/core'
import type { RemovableRef } from '@vueuse/core'

export type FeatureVisibilityKey = 'power' | 'webTerminal' | 'computerUse' | 'pasteText'
export type FeatureVisibility = Record<FeatureVisibilityKey, boolean>

const DEFAULT_FEATURE_VISIBILITY: FeatureVisibility = {
  power: true,
  webTerminal: true,
  computerUse: true,
  pasteText: true,
}

// Computer Use 的前端入口暂时下线。这里不读 localStorage 里的旧值，所以已经
// 打开过的浏览器也会隐藏；控制台按钮、设置开关和后端接口都保留，改回 true 即恢复。
export const computerUseEntryEnabled = false

const featureVisibility = useLocalStorage<FeatureVisibility>(
  'featureVisibility',
  DEFAULT_FEATURE_VISIBILITY,
  { mergeDefaults: true },
)

export function useFeatureVisibility(): RemovableRef<FeatureVisibility> {
  return featureVisibility
}
