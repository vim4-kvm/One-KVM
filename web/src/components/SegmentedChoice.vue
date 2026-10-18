<script setup lang="ts">
import { RadioGroupRoot, RadioGroupItem } from 'reka-ui'
defineProps<{ modelValue: string; label: string; options: { value: string; label: string }[]; disabled?: boolean }>()
defineEmits<{ 'update:modelValue': [value: string] }>()
</script>

<template>
  <RadioGroupRoot
    :model-value="modelValue"
    :aria-label="label"
    :disabled="disabled"
    orientation="horizontal"
    class="inline-flex max-w-full flex-wrap gap-1 rounded-lg bg-muted p-1"
    @update:model-value="(value) => value && $emit('update:modelValue', String(value))"
  >
    <RadioGroupItem
      v-for="option in options"
      :key="option.value"
      :value="option.value"
      class="min-h-8 rounded-md px-3 py-1.5 text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring disabled:opacity-50 data-[state=checked]:bg-background data-[state=checked]:shadow-sm"
    >
      {{ option.label }}
    </RadioGroupItem>
  </RadioGroupRoot>
</template>
