<script setup lang="ts">
import { ref } from 'vue'
import { RouterView, useRouter } from 'vue-router'
import { useThemeStore } from '@/stores/theme'
import LayoutSwitcher from '@/components/LayoutSwitcher.vue'
import UserProfile from '@/components/UserProfile.vue'
import LanguageSwitcher from '@/components/LanguageSwitcher.vue'
import { appMenuOptions, navigateByMenuKey } from '@/composables/useAppMenu'

const themeStore = useThemeStore()
const router = useRouter()
const collapsed = ref(false)
const appTitle = import.meta.env.VITE_APP_TITLE || '再保运维工具'

const handleUpdateValue = (key: string) => navigateByMenuKey(router, key)
</script>

<template>
  <n-layout class="h-100vh" has-sider>
    <n-layout-sider
      bordered
      collapse-mode="width"
      :collapsed-width="64"
      :width="240"
      :collapsed="collapsed"
      show-trigger
      @collapse="collapsed = true"
      @expand="collapsed = false"
    >
      <div class="h-16 flex items-center justify-center font-bold text-xl">
        <div class="i-carbon-cloud-service-management text-2xl mr-2 text-teal-600" />
        <span v-if="!collapsed">{{ appTitle }}</span>
      </div>
      <n-menu
        :collapsed="collapsed"
        :collapsed-width="64"
        :collapsed-icon-size="22"
        :options="appMenuOptions"
        @update:value="handleUpdateValue"
      />
    </n-layout-sider>
    <n-layout content-style="display: flex; flex-direction: column; min-height: 100%;">
      <n-layout-header bordered class="h-16 flex items-center justify-between px-6">
        <div class="text-lg font-medium">{{ $t('app.title') }} - {{ $t('app.footer') }}</div>
        <div class="flex items-center gap-4">
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
  </n-layout>
</template>