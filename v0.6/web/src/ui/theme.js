import { ref, watch } from 'vue'
const allowed = ['dark', 'white', 'gray']
function initialTheme() {
  try { const saved = localStorage.getItem('macross.theme'); return allowed.includes(saved) ? saved : 'white' }
  catch { return 'white' }
}
export const theme = ref(initialTheme())
watch(theme, (value) => {
  document.documentElement.dataset.theme = value
  document.documentElement.style.colorScheme = value === 'dark' ? 'dark' : 'light'
  document.querySelector('meta[name="theme-color"]')?.setAttribute('content', value === 'dark' ? '#141b27' : value === 'gray' ? '#e4e8ef' : '#f7f9fc')
  try { localStorage.setItem('macross.theme', value) } catch { /* 保存不可でも表示を切り替える */ }
}, { immediate: true })
