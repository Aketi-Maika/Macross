<script setup>
import { ref, computed, onMounted, onBeforeUnmount } from 'vue'
import { api } from '../api.js'
import { listRecordings, readRecording } from '../ui/fileBrowser.js'
import UiIcon from '../components/UiIcon.vue'
import ChannelBadge from '../components/ChannelBadge.vue'

const folders = ref([])
const items = ref([])
const channels = ref([])
const directory = ref('/')
const selected = ref(null)
const loading = ref(false)
const audioLoading = ref(false)
const downloading = ref(false)
const error = ref('')
const audioUrl = ref('')
let listingController = null
let audioController = null
let downloadController = null
let disposed = false
const folderName = computed(() => directory.value.split('/').filter(Boolean).join(' / ') || 'すべて')
const channelId = (name) => channels.value.find(c => c.name === name)?.id ?? null
const indexFile = computed(() => items.value.find(item => item.name === 'index.csv'))
const orderedItems = computed(() => [...items.value].sort((a, b) => Number(b.isDir) - Number(a.isDir) || String(b.modified).localeCompare(String(a.modified)) || a.name.localeCompare(b.name, 'ja')))
const join = (parent, name) => `${parent.replace(/\/$/, '')}/${name}`
const recordingType = (file) => file.isDir ? 'フォルダー' : /_tts(?:_\d+)?\.mp3$/i.test(file.name) ? '読み上げ' : /\.mp3$/i.test(file.name) ? 'PTT' : /\.csv$/i.test(file.name) ? 'CSV' : 'ファイル'
const isAudio = (file) => !file.isDir && /\.(mp3|wav|ogg)$/i.test(file.name)
const dateText = (value) => { const date = new Date(value); return Number.isNaN(date.getTime()) ? '—' : date.toLocaleString('ja-JP', { year: 'numeric', month: '2-digit', day: '2-digit', hour: '2-digit', minute: '2-digit' }) }
function sizeText(size) { return size >= 1048576 ? `${(size / 1048576).toFixed(1)} MB` : size >= 1024 ? `${Math.ceil(size / 1024)} KB` : `${size || 0} B` }
const recordingId = computed(() => selected.value?.name.match(/^\d{4}-\d{2}-\d{2}_\d{4}_(\d{4})(?:_tts)?(?:_\d+)?\.mp3$/i)?.[1] || '—')
function clearAudio() {
  audioController?.abort()
  if (audioUrl.value) URL.revokeObjectURL(audioUrl.value)
  audioUrl.value = ''
  audioLoading.value = false
}
async function openFolder(path) {
  listingController?.abort()
  const controller = new AbortController()
  listingController = controller
  clearAudio()
  selected.value = null
  directory.value = path
  items.value = []
  loading.value = true
  error.value = ''
  try {
    const result = await listRecordings(path, controller.signal)
    if (disposed || controller.signal.aborted) return
    items.value = result
    if (path === '/') folders.value = result.filter(item => item.isDir)
  } catch (e) {
    if (!disposed && e.name !== 'AbortError' && !controller.signal.aborted) error.value = e.message
  } finally { if (listingController === controller) loading.value = false }
}
function parentFolder() {
  const parts = directory.value.split('/').filter(Boolean)
  parts.pop()
  openFolder('/' + parts.join('/'))
}
async function choose(item) {
  if (item.isDir) return openFolder(join(directory.value, item.name))
  clearAudio()
  selected.value = item
  if (!isAudio(item)) return
  const controller = new AbortController()
  audioController = controller
  audioLoading.value = true
  error.value = ''
  try {
    const blob = await readRecording(join(directory.value, item.name), controller.signal)
    if (!disposed && !controller.signal.aborted) audioUrl.value = URL.createObjectURL(blob)
  } catch (e) {
    if (!disposed && e.name !== 'AbortError' && !controller.signal.aborted) error.value = e.message
  } finally { if (audioController === controller) audioLoading.value = false }
}
async function download(item) {
  if (downloading.value) return
  downloading.value = true
  error.value = ''
  downloadController = new AbortController()
  try {
    const blob = await readRecording(join(directory.value, item.name), downloadController.signal)
    if (disposed) return
    const url = URL.createObjectURL(blob)
    const anchor = document.createElement('a')
    anchor.href = url
    anchor.download = item.name
    document.body.appendChild(anchor)
    anchor.click()
    anchor.remove()
    setTimeout(() => URL.revokeObjectURL(url), 1000)
  } catch (e) { if (!disposed && e.name !== 'AbortError') error.value = e.message }
  finally { downloading.value = false }
}
onMounted(async () => {
  api.get('/api/admin/channels').then(data => { if (!disposed) channels.value = data.channels }).catch(() => {})
  await openFolder('/')
})
onBeforeUnmount(() => { disposed = true; listingController?.abort(); downloadController?.abort(); clearAudio() })
</script>

<template>
  <div class="container recordings-page">
    <p v-if="error" class="error" role="alert">{{ error }} <a href="/files/" target="_blank" rel="noopener">元の録音一覧を開く</a></p>
    <div class="recordings-layout">
      <aside class="panel folders-panel"><h2>チャンネル</h2><button class="folder-button" :class="{ selected: directory === '/' }" @click="openFolder('/')"><UiIcon name="folder" />すべて</button><button v-for="folder in folders" :key="folder.name" class="folder-button" :class="{ selected: directory === '/' + folder.name }" @click="openFolder('/' + folder.name)"><ChannelBadge v-if="channelId(folder.name) != null" :id="channelId(folder.name)" /><UiIcon v-else name="folder" /><strong>{{ folder.name }}</strong><UiIcon name="chevron" /></button><p v-if="!loading && !folders.length" class="muted">録音フォルダーはまだありません。</p></aside>
      <section class="panel files-panel" :aria-busy="loading">
        <div class="files-heading"><h2><UiIcon name="folder" />録音 / {{ folderName }}</h2><button class="secondary small" aria-label="録音一覧を更新" :disabled="loading" @click="openFolder(directory)"><UiIcon name="refresh" /></button></div>
        <button v-if="directory !== '/'" class="secondary small parent-button" @click="parentFolder">上のフォルダーへ</button>
        <div v-if="loading" class="empty-state" role="status">録音を読み込んでいます…</div>
        <div v-else-if="!items.length" class="empty-state">{{ error ? '一覧を表示できませんでした。' : 'このフォルダーに録音はありません。' }}</div>
        <div v-else class="table-wrap"><table><thead><tr><th>ファイル名</th><th>種類</th><th>更新日時</th><th>サイズ</th><th><span class="sr-only">ダウンロード</span></th></tr></thead><tbody><tr v-for="item in orderedItems" :key="item.name" :class="{ chosen: selected?.name === item.name }"><td><button class="file-select" @click="choose(item)"><UiIcon :name="item.isDir ? 'folder' : 'file'" /><span>{{ item.name }}</span></button></td><td>{{ recordingType(item) }}</td><td class="file-date">{{ dateText(item.modified) }}</td><td>{{ item.isDir ? '—' : sizeText(item.size) }}</td><td><button v-if="!item.isDir" class="secondary small" :aria-label="item.name + ' をダウンロード'" :disabled="downloading" @click="download(item)"><UiIcon name="download" /></button></td></tr></tbody></table></div>
        <div class="files-note"><button v-if="indexFile" class="secondary" :disabled="downloading" @click="download(indexFile)"><UiIcon name="download" />index.csvをダウンロード</button><p class="muted"><UiIcon name="info" /> 録音はチャンネルごとのフォルダーに保存されます。</p></div>
      </section>
      <aside class="panel player-panel"><h2>録音の再生</h2><div v-if="!selected" class="empty-state"><UiIcon name="mic" /><p>一覧から録音を選択してください。</p></div><template v-else><div class="selected-file"><ChannelBadge :id="channelId(directory.split('/').filter(Boolean)[0])" /><h3>{{ selected.name }}</h3></div><p class="muted">チャンネル：{{ folderName }}</p><p v-if="audioLoading" role="status" class="muted">音声を読み込んでいます…</p><audio v-else-if="audioUrl" :src="audioUrl" controls preload="metadata" :aria-label="selected.name + ' の再生'" @error="error = '音声を再生できません。ダウンロードして確認してください。'" /><p v-else-if="!isAudio(selected)" class="muted">このファイルはダウンロードして確認できます。</p><dl class="file-details"><dt>ID</dt><dd>{{ recordingId }}</dd><dt>種別</dt><dd>{{ recordingType(selected) }}</dd><dt>更新日時</dt><dd>{{ dateText(selected.modified) }}</dd><dt>サイズ</dt><dd>{{ sizeText(selected.size) }}</dd></dl><button class="download-selected" :disabled="downloading" @click="download(selected)"><UiIcon name="download" />{{ downloading ? '準備中…' : 'ダウンロード' }}</button></template></aside>
    </div>
  </div>
</template>

<style scoped>
.recordings-layout { display: grid; grid-template-columns: 220px minmax(0, 1fr) 320px; gap: 18px; align-items: stretch; }
.folders-panel { padding: 20px 14px; }
.folders-panel h2 { padding: 0 8px; }
.folder-button { display: flex; justify-content: flex-start; gap: 12px; width: 100%; padding: 12px; margin-bottom: 10px; color: var(--text); background: transparent; text-align: left; border: 1px solid transparent; }
.folder-button.selected, .folder-button:hover { background: var(--selected); border-color: var(--line); }
.folder-button strong { overflow-wrap: anywhere; flex: 1; }
.folder-button .channel-badge { width: 46px; height: 46px; }
.files-panel { display: flex; flex-direction: column; min-height: 560px; }
.files-heading { display: flex; align-items: center; justify-content: space-between; gap: 12px; margin-bottom: 20px; }
.files-heading h2 { margin: 0; overflow-wrap: anywhere; }
.parent-button { align-self: flex-start; margin-bottom: 12px; }
.file-select { padding: 4px 0; justify-content: flex-start; color: var(--text); background: transparent; text-align: left; font-weight: 500; min-width: 170px; }
.file-select:hover:not(:disabled) { background: transparent; text-decoration: underline; }
.file-select span { overflow-wrap: anywhere; }
/* 録音が多くても再生欄が見えるよう、一覧は枠内でスクロールさせ、見出し行を固定する */
.table-wrap { position: relative; max-height: min(65vh, 720px); overflow: auto; }
thead th { position: sticky; top: 0; z-index: 1; }
.folders-panel, .player-panel { align-self: start; position: sticky; top: 16px; }
table { font-size: .85rem; }
th, td { padding: 10px 8px; }
td:not(:first-child) { white-space: nowrap; }
.file-date { font-size: .8rem; }
.chosen { background: var(--selected); }
.files-note { margin-top: auto; padding-top: 32px; }
.files-note button { width: 100%; }
.files-note p { margin: 16px 0 0; }
.selected-file { display: flex; align-items: center; gap: 14px; margin-bottom: 18px; }
.selected-file h3 { min-width: 0; margin: 0; overflow-wrap: anywhere; }
.player-panel audio { width: 100%; margin: 20px 0; }
.file-details { display: grid; grid-template-columns: 80px minmax(0, 1fr); font-size: .9rem; margin: 20px 0 36px; }
.file-details dt, .file-details dd { margin: 0; padding: 9px; border-bottom: 1px solid var(--line); }
.file-details dt { background: var(--soft); }
.file-details dd { overflow-wrap: anywhere; }
.download-selected { width: 100%; }
.empty-state .ui-icon { margin: auto; font-size: 2rem; }
.sr-only { position: absolute; width: 1px; height: 1px; overflow: hidden; clip: rect(0,0,0,0); }
@media (max-width: 1200px) { .recordings-layout { grid-template-columns: 200px minmax(0, 1fr); } .player-panel { grid-column: 2; } .folders-panel { grid-row: span 2; } }
@media (max-width: 700px) { .folders-panel, .player-panel { position: static; } .table-wrap { max-height: 55vh; } .recordings-layout { grid-template-columns: 1fr; } .folders-panel, .player-panel { grid-column: 1; grid-row: auto; } .files-panel { min-height: 300px; } .folder-button { display: inline-flex; width: auto; margin-right: 8px; } }
</style>
