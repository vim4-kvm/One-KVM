import type { Composer } from 'vue-i18n'

export function formatDetail(detail: string | null | undefined, t: Composer['t']): string {
  if (!detail) return ''
  const characters = /^(\d+) chars$/.exec(detail)
  if (characters) return t('extensions.mcp.detail.characters', { count: characters[1] })
  const dimensions = /^(\d+)x(\d+)$/.exec(detail)
  if (dimensions) return `${dimensions[1]}×${dimensions[2]}`
  const scroll = /^(up|down|left|right) (\d+)$/.exec(detail)
  if (scroll) return t(`extensions.mcp.detail.${scroll[1]}`, { count: scroll[2] })
  return detail
}
