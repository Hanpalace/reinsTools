import { defineStore } from 'pinia'
import { ref } from 'vue'

export const useThemeStore = defineStore('theme', () => {
  const isDark = ref(false)

  function toggleTheme() {
    isDark.value = !isDark.value
    // 同步 html 上的 dark class，使 UnoCSS 的 dark: 变体跟随应用主题
    document.documentElement.classList.toggle('dark', isDark.value)
  }

  return { isDark, toggleTheme }
})