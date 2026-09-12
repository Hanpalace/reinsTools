// 应用菜单的唯一定义：两个布局（侧边栏 / 顶部导航）共用。
// 叶子菜单直接携带路由 path，跳转时原样 push；父级无 path，
// 点击父级不导航（避免 key 字符串手术产生不存在的路由）。
import { h } from 'vue'
import type { Router } from 'vue-router'
import { t } from '@/locales'

// 菜单层级：顶级加粗 + teal 图标，子级浅灰弱化
const topLabel = (key: string) => () => h('span', { class: 'font-semibold' }, t(key))
const childLabel = (key: string) => () => h('span', { class: 'text-gray-500 dark:text-gray-400' }, t(key))

const renderIcon = (icon: string) => () => h('div', { class: icon })

interface AppMenuItem {
  label: () => ReturnType<typeof h>
  key: string
  icon?: () => ReturnType<typeof h>
  path?: string
  children?: AppMenuItem[]
}

export const appMenuOptions: AppMenuItem[] = [
  {
    label: topLabel('menu.home'),
    key: 'home',
    path: '/',
    icon: renderIcon('i-carbon-home text-teal-600'),
  },
  {
    label: topLabel('menu.dataBatchImport'),
    key: 'data-batch-import-group',
    icon: renderIcon('i-carbon-document-import text-teal-600'),
    children: [
      {
        label: childLabel('menu.dataConnection'),
        key: 'data-connection',
        path: '/data/connection',
      },
      {
        label: childLabel('menu.dataImport'),
        key: 'data-batch-import',
        path: '/data/batch-import',
      },
      {
        label: childLabel('menu.importLog'),
        key: 'data-import-log',
        path: '/data/import-log',
      },
    ],
  },
]

/** 按菜单 key 导航：仅叶子（带 path）生效，父级 key 直接忽略 */
export const navigateByMenuKey = (router: Router, key: string) => {
  for (const item of appMenuOptions) {
    if (item.path && item.key === key) {
      router.push(item.path)
      return
    }
    for (const child of item.children ?? []) {
      if (child.path && child.key === key) {
        router.push(child.path)
        return
      }
    }
  }
}
