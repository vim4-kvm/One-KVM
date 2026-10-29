<script setup lang="ts">
import type { ConsoleLayout } from '@/composables/useConsoleLayout'

defineProps<{ layout: ConsoleLayout }>()
</script>

<template>
  <svg viewBox="0 0 240 96" class="block h-24 w-full rounded-md border bg-background text-foreground" aria-hidden="true">
    <rect width="240" height="96" fill="#09090b" />
    <path d="M30 86 84 35l35 30 26-20 48 41Z" fill="none" stroke="#71717a" opacity=".35" />
    <g v-if="layout !== 'floating'">
      <rect width="240" height="12" fill="var(--background)" />
      <path d="M0 12h240" stroke="currentColor" opacity=".2" />
      <rect x="4" y="3" width="6" height="6" rx="1" fill="currentColor" opacity=".6" />
      <circle cx="16" cy="6" r="2" fill="#22c55e" />
      <path d="M21 6h25" stroke="currentColor" opacity=".5" />
      <g v-if="layout === 'current'" stroke="currentColor" opacity=".5">
        <path d="M51 2v8m7-6v4m10-4v4m10-4v4m10-4v4m10-4v4m10-4v4m25-4v4m10-4v4m10-4v4m10-4v4m10-4v4m10-4v4" />
      </g>
      <circle cx="232" cy="6" r="1.5" fill="currentColor" />
    </g>
    <g v-if="layout === 'sidebar'">
      <rect y="12" width="16" height="84" fill="var(--background)" />
      <path d="M16 12v84" stroke="currentColor" opacity=".2" />
      <path d="M5 22h6m-6 10h6m-6 10h6m-6 10h6m-6 10h6m-6 20h6" stroke="currentColor" opacity=".5" />
    </g>
    <g v-if="layout === 'floating'">
      <rect x="93" y="3" width="54" height="9" rx="4.5" fill="var(--background)" stroke="currentColor" stroke-opacity=".2" />
      <circle cx="100" cy="7.5" r="1.5" fill="#22c55e" />
      <path d="M105 7.5h27m6-1 2 2 2-2" fill="none" stroke="currentColor" opacity=".5" />
    </g>
  </svg>
</template>
