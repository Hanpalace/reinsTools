<script setup lang="ts">
import { RouterView, useRouter } from 'vue-router'
import { useThemeStore } from '@/stores/theme'
import LayoutSwitcher from '@/components/LayoutSwitcher.vue'
import UserProfile from '@/components/UserProfile.vue'
import LanguageSwitcher from '@/components/LanguageSwitcher.vue'
import { appMenuOptions, navigateByMenuKey } from '@/composables/useAppMenu'

const themeStore = useThemeStore()
const router = useRouter()
const appTitle = import.meta.env.VITE_APP_TITLE || '再保运维工具'

const handleUpdateValue = (key: string) => navigateByMenuKey(router, key)
</script>

<template>
  <n-layout class="h-100vh" content-style="display: flex; flex-direction: column;">
    <n-layout-header bordered class="h-16 flex items-center justify-between px-6">
      <div class="flex items-center gap-8">
        <div class="flex items-center font-bold text-xl">
          <div class="i-carbon-cloud-service-management text-2xl mr-2 text-teal-600" />
          <span>{{ appTitle }}</span>
        </div>
        <n-menu
          mode="horizontal"
          :options="appMenuOptions"
          @update:value="handleUpdateValue"
        />
      </div>
      <div class="flex items-center gap-4">
        <slot name="header-extra"></slot>
        <LayoutSwitcher />
        <LanguageSwitcher />
        <button class="icon-btn i-carbon-sun dark:i-carbon-moon text-xl" @click="themeStore.toggleTheme" />
        <UserProfile />
      </div>
    </n-layout-header>
    <n-layout-content class="flex-1" content-style="padding: 24px;">
      <RouterView />
    </n-layout-content>
    <n-layout-footer bordered class="p-4 text-center text-gray-500 text-sm">
      <div>{{ $t('app.footer') }}</div>
      <div>{{ $t('app.icp') }}</div>
    </n-layout-footer>
  </n-layout>
</template>