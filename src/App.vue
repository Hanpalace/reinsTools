<script setup lang="ts">
import { zhCN, dateZhCN, enUS, dateEnUS, darkTheme, type GlobalThemeOverrides } from 'naive-ui'
import { computed } from 'vue'
import { useI18n } from 'vue-i18n'
import { useThemeStore } from '@/stores/theme'
import { useLayoutStore } from '@/stores/layout'
import MainLayout from '@/layouts/MainLayout.vue'
import TopMenuLayout from '@/layouts/TopMenuLayout.vue'

const themeStore = useThemeStore()
const layoutStore = useLayoutStore()
const { locale } = useI18n()

const theme = computed(() => themeStore.isDark ? darkTheme : null)
const naiveLocale = computed(() => locale.value === 'zh-CN' ? zhCN : enUS)
const naiveDateLocale = computed(() => locale.value === 'zh-CN' ? dateZhCN : dateEnUS)

// 全局主题定制：
// - 品牌青绿主色（与现有 teal 强调色一致）
// - 更圆润的圆角（按钮 / 卡片 / 输入框等）
// - 内容区灰色背景 + 白色侧边栏，形成模块层次感
const themeOverrides = computed<GlobalThemeOverrides>(() => ({
  common: {
    primaryColor: '#0d9488',
    primaryColorHover: '#0f766e',
    primaryColorPressed: '#115e59',
    primaryColorSuppl: '#0d9488',
    borderRadius: '10px',
    borderRadiusSmall: '6px',
  },
  Button: {
    borderRadiusMedium: '10px',
    borderRadiusSmall: '8px',
    borderRadiusLarge: '12px',
  },
  Card: {
    borderRadius: '12px',
  },
  Layout: {
    color: themeStore.isDark ? '#111827' : '#f3f4f6',
    siderColor: themeStore.isDark ? '#1b1b1f' : '#ffffff',
  },
}))
</script>

<template>
  <n-config-provider
    :locale="naiveLocale"
    :date-locale="naiveDateLocale"
    :theme="theme"
    :theme-overrides="themeOverrides"
  >
    <n-global-style />
    <n-loading-bar-provider>
      <n-message-provider>
        <n-notification-provider>
          <n-dialog-provider>
            <component :is="layoutStore.currentLayout === 'sider' ? MainLayout : TopMenuLayout" />
          </n-dialog-provider>
        </n-notification-provider>
      </n-message-provider>
    </n-loading-bar-provider>
  </n-config-provider>
</template>
