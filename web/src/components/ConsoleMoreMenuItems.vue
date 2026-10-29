<script setup lang="ts">
import { useI18n } from 'vue-i18n'
import { Sun, Moon, KeyRound, LogOut } from 'lucide-vue-next'
import LanguageToggleButton from '@/components/LanguageToggleButton.vue'
import { DropdownMenuItem, DropdownMenuSeparator } from '@/components/ui/dropdown-menu'

defineProps<{ isDark: boolean; managed: boolean; user?: string | null }>()
const emit = defineEmits<{
  (event: 'toggleTheme'): void
  (event: 'changePassword'): void
  (event: 'logout'): void
}>()
const { t } = useI18n()
</script>

<template>
  <DropdownMenuItem @select="emit('toggleTheme')">
    <Sun v-if="isDark" class="size-4" /><Moon v-else class="size-4" />
    {{ isDark ? t('settings.lightMode') : t('settings.darkMode') }}
  </DropdownMenuItem>
  <DropdownMenuItem as-child class="p-0">
    <LanguageToggleButton label-mode="next" size="sm" variant="ghost"
      class="w-full justify-start rounded-sm px-2 py-1.5 font-normal shadow-none" />
  </DropdownMenuItem>
  <template v-if="!managed">
    <DropdownMenuSeparator />
    <DropdownMenuItem disabled>{{ user || 'admin' }}</DropdownMenuItem>
    <DropdownMenuItem @select="emit('changePassword')"><KeyRound class="size-4" />{{ t('auth.changePassword') }}</DropdownMenuItem>
    <DropdownMenuItem @select="emit('logout')"><LogOut class="size-4" />{{ t('auth.logout') }}</DropdownMenuItem>
  </template>
</template>
