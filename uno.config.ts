import { defineConfig, presetUno, presetAttributify, presetIcons } from 'unocss'
import carbonIcons from '@iconify-json/carbon/icons.json'

export default defineConfig({
  presets: [
    presetUno({ dark: 'class' }),
    presetAttributify(),
    presetIcons({
      scale: 1.2,
      warn: true,
      // 显式提供 carbon 集合：pnpm 下 unocss 无法自动解析 @iconify/json，
      // 否则所有 i-carbon-* 图标加载失败（表现为空白占位）。
      // 注意：@iconify/utils 3.x 直接传 IconifyJSON 不生效（getCustomIcon 按
      // 图标名索引集合对象），需用函数形式返回，才会走 searchForIcon 解析。
      collections: {
        carbon: () => carbonIcons,
      },
    }),
  ],
  shortcuts: [
    ['btn', 'px-4 py-1 rounded inline-block bg-teal-600 text-white cursor-pointer hover:bg-teal-700 disabled:cursor-default disabled:bg-gray-600 disabled:opacity-50'],
    ['icon-btn', 'text-[0.9em] inline-block cursor-pointer select-none opacity-75 transition duration-200 ease-in-out hover:opacity-100 hover:text-teal-600']
  ],
})