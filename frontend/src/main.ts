// @env browser
import '@codex-proxy/ui/styles.css'
import './styles/index.css'
import { createApp } from 'vue'
import App from './App.vue'

function syncTheme() {
  const host = window.codexProxyPlugin
  const theme = host?.theme === 'dark' ? 'dark' : 'light'
  document.documentElement.dataset.theme = theme
  document.documentElement.style.colorScheme = theme
}

syncTheme()
window.addEventListener('codex-proxy-themechange', syncTheme)

createApp(App).mount('#app')
