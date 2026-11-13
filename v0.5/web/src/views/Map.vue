<script setup>
/**
 * 管理者向けの地図（3D）。接続中の人の現在地と、発信（PTT / 読み上げ）した場所を表示する。
 *
 * - 背景: 地理院タイル（標準地図 / 航空写真）
 * - 建物: OpenFreeMap（OpenMapTiles）のベクタタイルを高さ付きで立体表示
 * - 地形: Terrarium 形式の標高タイル
 * - 人: 発言中は赤、受信中は緑の柱（高さで状態が分かる）とラベル
 */
import { ref, reactive, computed, onMounted, onBeforeUnmount, shallowRef } from 'vue'
import maplibregl from 'maplibre-gl'
import 'maplibre-gl/dist/maplibre-gl.css'
import { api } from '../api.js'
import UiIcon from '../components/UiIcon.vue'

const POLL_MS = 3000
const STALE_MS = 2 * 60 * 1000 // これより古い位置は灰色
const TOKYO = [139.767, 35.681]

const mapEl = ref(null)
const map = shallowRef(null)
const error = ref('')
const enabled = ref(true)
const users = ref([])
const history = ref([])
const serverNow = ref(Date.now())
const opt = reactive({ threeD: true, photo: false, terrain: true, historyHours: 24, buildings: true })
const selected = ref(null)

const markers = new Map() // id -> maplibregl.Marker
let timer = null
let fitted = false
let mapCfg = null
let ready = false // スタイルの読み込みが終わった（isStyleLoaded はタイル読み込み中も false になるため使わない）

const located = computed(() => users.value.filter((u) => u.location))
const shownHistory = computed(() => {
  if (opt.historyHours === 0) return []
  const since = opt.historyHours < 0 ? 0 : serverNow.value - opt.historyHours * 3600 * 1000
  return history.value.filter((h) => h.ts >= since)
})
const recentHistory = computed(() => [...shownHistory.value].reverse().slice(0, 50))

function ageText(ts) {
  const s = Math.max(0, Math.round((serverNow.value - ts) / 1000))
  if (s < 60) return `${s}秒前`
  if (s < 3600) return `${Math.floor(s / 60)}分前`
  return `${Math.floor(s / 3600)}時間前`
}

function stateOf(u) {
  if (!u.location) return 'none'
  if (serverNow.value - u.location.ts > STALE_MS) return 'stale'
  return u.talking ? 'talk' : 'listen'
}

const COLORS = { talk: '#e5484d', listen: '#30a46c', stale: '#8b94a3', none: '#8b94a3' }

/** 地点を中心とした半径 r メートルの円（柱の断面・誤差円） */
function circle(lon, lat, r, n = 20) {
  const pts = []
  const dLat = r / 111320
  const dLon = r / (111320 * Math.cos((lat * Math.PI) / 180))
  for (let i = 0; i <= n; i++) {
    const a = (i / n) * 2 * Math.PI
    pts.push([lon + dLon * Math.cos(a), lat + dLat * Math.sin(a)])
  }
  return [pts]
}

function buildStyle(m) {
  const sources = {
    base: {
      type: 'raster',
      tiles: [m.base_tiles],
      tileSize: 256,
      maxzoom: 18,
      attribution: '<a href="https://maps.gsi.go.jp/development/ichiran.html" target="_blank" rel="noopener">地理院タイル</a>',
    },
    photo: {
      type: 'raster',
      tiles: [m.photo_tiles],
      tileSize: 256,
      maxzoom: 18,
      attribution: '<a href="https://maps.gsi.go.jp/development/ichiran.html" target="_blank" rel="noopener">地理院タイル</a>',
    },
    accuracy: { type: 'geojson', data: empty() },
    history: { type: 'geojson', data: empty() },
    pillars: { type: 'geojson', data: empty() },
  }
  if (m.terrain_tiles) {
    sources.terrain = {
      type: 'raster-dem',
      tiles: [m.terrain_tiles],
      encoding: 'terrarium',
      tileSize: 256,
      maxzoom: 15,
      attribution: '標高: Mapzen / AWS Terrain Tiles',
    }
  }
  if (m.buildings_tilejson) sources.buildings = { type: 'vector', url: m.buildings_tilejson }

  const layers = [
    { id: 'bg', type: 'background', paint: { 'background-color': '#dfe3e8' } },
    { id: 'base', type: 'raster', source: 'base' },
    { id: 'photo', type: 'raster', source: 'photo', layout: { visibility: 'none' } },
    {
      id: 'accuracy',
      type: 'fill',
      source: 'accuracy',
      paint: { 'fill-color': ['get', 'color'], 'fill-opacity': 0.12 },
    },
    {
      id: 'history',
      type: 'circle',
      source: 'history',
      paint: {
        'circle-radius': ['interpolate', ['linear'], ['zoom'], 8, 3, 16, 7],
        'circle-color': ['match', ['get', 'kind'], 'TTS', '#3d8bfd', '#f5a524'],
        'circle-opacity': ['get', 'opacity'],
        'circle-stroke-color': '#fff',
        'circle-stroke-width': 1,
        'circle-stroke-opacity': ['get', 'opacity'],
        'circle-pitch-alignment': 'map',
      },
    },
  ]
  if (sources.buildings) {
    layers.push({
      id: 'buildings',
      type: 'fill-extrusion',
      source: 'buildings',
      'source-layer': 'building',
      minzoom: 14,
      paint: {
        'fill-extrusion-color': '#c9ced6',
        'fill-extrusion-height': ['coalesce', ['get', 'render_height'], 8],
        'fill-extrusion-base': ['coalesce', ['get', 'render_min_height'], 0],
        'fill-extrusion-opacity': 0.75,
      },
    })
  }
  layers.push({
    id: 'pillars',
    type: 'fill-extrusion',
    source: 'pillars',
    paint: {
      'fill-extrusion-color': ['get', 'color'],
      'fill-extrusion-height': ['get', 'height'],
      'fill-extrusion-base': 0,
      'fill-extrusion-opacity': 0.9,
    },
  })
  return { version: 8, sources, layers }
}

function empty() {
  return { type: 'FeatureCollection', features: [] }
}

/* ---------------- 地図の更新 ---------------- */

function markerEl(u) {
  const el = document.createElement('div')
  el.className = 'pin'
  el.innerHTML = '<span class="pin-dot"></span><span class="pin-label"></span>'
  el.addEventListener('click', (e) => {
    e.stopPropagation()
    focusUser(u.id)
  })
  return el
}

function render() {
  const m = map.value
  if (!m || !ready) return
  const seen = new Set()
  const pillars = []
  const acc = []
  for (const u of located.value) {
    const { lat, lon, accuracy } = u.location
    const st = stateOf(u)
    const color = COLORS[st]
    seen.add(u.id)
    let mk = markers.get(u.id)
    if (!mk) {
      // 地形の陰になっても薄くしない（既定では 0.2 になり、引いた表示でほぼ見えなくなる）
      mk = new maplibregl.Marker({ element: markerEl(u), anchor: 'bottom', opacity: '1', opacityWhenCovered: '1' }).setLngLat([lon, lat]).addTo(m)
      markers.set(u.id, mk)
    }
    mk.setLngLat([lon, lat])
    const el = mk.getElement()
    el.dataset.state = st
    el.classList.toggle('selected', selected.value === u.id)
    el.querySelector('.pin-label').textContent = u.talking ? `${u.name} 🎙 ${u.talking}` : u.name
    const r = 4
    pillars.push({
      type: 'Feature',
      properties: { color, height: st === 'talk' ? 120 : st === 'listen' ? 45 : 15 },
      geometry: { type: 'Polygon', coordinates: circle(lon, lat, r, 16) },
    })
    if (accuracy && accuracy > 5 && accuracy < 5000) {
      acc.push({ type: 'Feature', properties: { color }, geometry: { type: 'Polygon', coordinates: circle(lon, lat, accuracy, 40) } })
    }
  }
  for (const [id, mk] of markers) {
    if (!seen.has(id)) {
      mk.remove()
      markers.delete(id)
    }
  }
  const span = Math.max(1, opt.historyHours > 0 ? opt.historyHours * 3600 * 1000 : 24 * 3600 * 1000)
  const hist = shownHistory.value.map((h) => ({
    type: 'Feature',
    properties: { kind: h.kind, opacity: Math.max(0.25, 1 - (serverNow.value - h.ts) / span) },
    geometry: { type: 'Point', coordinates: [h.location.lon, h.location.lat] },
  }))
  m.getSource('pillars')?.setData({ type: 'FeatureCollection', features: pillars })
  m.getSource('accuracy')?.setData({ type: 'FeatureCollection', features: acc })
  m.getSource('history')?.setData({ type: 'FeatureCollection', features: hist })
  if (!fitted && located.value.length) {
    fitted = true
    fitAll(false)
  }
}

function fitAll(animate = true) {
  const m = map.value
  const pts = located.value.map((u) => [u.location.lon, u.location.lat])
  if (!m || !pts.length) return
  if (pts.length === 1) {
    m.easeTo({ center: pts[0], zoom: 16, duration: animate ? 800 : 0 })
    return
  }
  const b = new maplibregl.LngLatBounds(pts[0], pts[0])
  pts.forEach((p) => b.extend(p))
  m.fitBounds(b, { padding: { top: 110, bottom: 70, left: 90, right: 90 }, maxZoom: 17, duration: animate ? 800 : 0 })
}

function focusUser(id) {
  selected.value = id
  const u = users.value.find((x) => x.id === id)
  if (u?.location) flyTo(u.location)
  render()
}

function flyTo(loc) {
  map.value?.flyTo({ center: [loc.lon, loc.lat], zoom: 17, pitch: opt.threeD ? 60 : 0, duration: 1200 })
}

/* ---------------- 表示の切り替え ---------------- */

function applyOptions() {
  const m = map.value
  if (!m || !ready) return
  m.setLayoutProperty('base', 'visibility', opt.photo ? 'none' : 'visible')
  m.setLayoutProperty('photo', 'visibility', opt.photo ? 'visible' : 'none')
  if (m.getLayer('buildings')) m.setLayoutProperty('buildings', 'visibility', opt.buildings && opt.threeD ? 'visible' : 'none')
  m.setLayoutProperty('pillars', 'visibility', opt.threeD ? 'visible' : 'none')
  if (m.getSource('terrain')) m.setTerrain(opt.terrain && opt.threeD ? { source: 'terrain', exaggeration: 1.2 } : null)
  render()
}

function toggle3d() {
  opt.threeD = !opt.threeD
  map.value?.easeTo({ pitch: opt.threeD ? 55 : 0, bearing: opt.threeD ? map.value.getBearing() : 0, duration: 600 })
  applyOptions()
}

/* ---------------- データ ---------------- */

async function load() {
  try {
    const r = await api.get('/api/admin/locations')
    enabled.value = r.enabled
    users.value = r.users
    history.value = r.history
    serverNow.value = r.now
    if (!mapCfg) {
      mapCfg = r.map
      initMap()
    }
    error.value = ''
    render()
  } catch (e) {
    error.value = e.message
  }
}

function initMap() {
  const m = new maplibregl.Map({
    container: mapEl.value,
    style: buildStyle(mapCfg),
    center: TOKYO,
    zoom: 5,
    pitch: 55,
    bearing: -15,
    maxPitch: 80,
    attributionControl: { compact: true },
  })
  m.addControl(new maplibregl.NavigationControl({ visualizePitch: true }), 'top-right')
  m.addControl(new maplibregl.ScaleControl({ unit: 'metric' }), 'bottom-left')
  m.addControl(new maplibregl.FullscreenControl(), 'top-right')
  m.on('load', () => {
    ready = true
    applyOptions()
    render()
  })
  m.on('click', () => {
    selected.value = null
    render()
  })
  // タイルが取れないなどのエラーは地図の表示を止めない
  m.on('error', (e) => console.warn('map:', e?.error?.message || e))
  map.value = m
}

onMounted(() => {
  load()
  timer = setInterval(load, POLL_MS)
})

onBeforeUnmount(() => {
  clearInterval(timer)
  markers.forEach((mk) => mk.remove())
  markers.clear()
  map.value?.remove()
})
</script>

<template>
  <div class="container map-page">
    <div class="map-wrap">
      <div ref="mapEl" class="map"></div>
      <div class="toolbar">
        <button class="secondary small" @click="fitAll()"><UiIcon name="user" />全員を表示</button>
        <button class="secondary small" :class="{ active: opt.threeD }" @click="toggle3d">{{ opt.threeD ? '3D' : '2D' }}</button>
        <button class="secondary small" :class="{ active: opt.photo }" @click="(opt.photo = !opt.photo), applyOptions()">航空写真</button>
        <button
          class="secondary small"
          :class="{ active: opt.terrain }"
          :disabled="!opt.threeD"
          @click="(opt.terrain = !opt.terrain), applyOptions()"
        >
          地形
        </button>
        <button
          class="secondary small"
          :class="{ active: opt.buildings }"
          :disabled="!opt.threeD"
          @click="(opt.buildings = !opt.buildings), applyOptions()"
        >
          建物
        </button>
        <select v-model.number="opt.historyHours" class="small-select" aria-label="発信履歴の表示期間" @change="render">
          <option :value="0">発信履歴: 表示しない</option>
          <option :value="1">発信履歴: 1時間</option>
          <option :value="24">発信履歴: 24時間</option>
          <option :value="-1">発信履歴: すべて</option>
        </select>
      </div>
      <div class="legend">
        <span><i style="background: #e5484d"></i>発言中</span>
        <span><i style="background: #30a46c"></i>受信中</span>
        <span><i style="background: #8b94a3"></i>2分以上更新なし</span>
        <span><i class="round" style="background: #f5a524"></i>発信した場所</span>
        <span><i class="round" style="background: #3d8bfd"></i>読み上げした場所</span>
      </div>
    </div>

    <aside class="side">
      <p v-if="error" class="error">{{ error }}</p>
      <p v-if="!enabled" class="error">Macross.yaml の location.enabled が false のため、位置情報は集めていません。</p>
      <h2>接続中（{{ users.length }}人 / 位置あり {{ located.length }}人）</h2>
      <ul class="list">
        <li
          v-for="u in users"
          :key="u.id"
          :class="{ selected: selected === u.id, nolocation: !u.location }"
          :tabindex="u.location ? 0 : -1"
          role="button"
          :aria-disabled="!u.location"
          @click="u.location && focusUser(u.id)"
          @keydown.enter="u.location && focusUser(u.id)"
          @keydown.space.prevent="u.location && focusUser(u.id)"
        >
          <span class="state" :style="{ background: COLORS[stateOf(u)] }"></span>
          <div class="grow">
            <div>
              {{ u.name }} <span class="muted">({{ u.id }})</span>
              <span v-if="u.superuser" class="tag su">SU</span>
              <span v-else-if="u.admin" class="tag">管理者</span>
            </div>
            <div class="muted">
              <template v-if="u.talking">🎙 発言中: {{ u.talking }}</template>
              <template v-else-if="u.listening.length">受信: {{ u.listening.join(' / ') }}</template>
              <template v-else>チャンネル未参加</template>
            </div>
            <div class="muted">
              <template v-if="u.location">
                {{ ageText(u.location.ts) }}
                <template v-if="u.location.accuracy"> / 誤差 ±{{ Math.round(u.location.accuracy) }}m</template>
                <template v-if="u.location.alt != null"> / 高度 {{ Math.round(u.location.alt) }}m</template>
              </template>
              <template v-else>位置情報なし（未許可・未対応）</template>
            </div>
          </div>
        </li>
        <li v-if="!users.length" class="muted">接続している人はいません</li>
      </ul>

      <h2>発信履歴（新しい順・最大50件）</h2>
      <ul class="list history">
        <li v-for="h in recentHistory" :key="h.ts + h.id" tabindex="0" role="button" @click="flyTo(h.location)" @keydown.enter="flyTo(h.location)" @keydown.space.prevent="flyTo(h.location)">
          <span class="state round" :style="{ background: h.kind === 'TTS' ? '#3d8bfd' : '#f5a524' }"></span>
          <div class="grow">
            <div>{{ h.name }} <span class="muted">({{ h.id }}) {{ h.channel }} {{ h.kind === 'TTS' ? '読み上げ' : '' }}</span></div>
            <div class="muted">{{ h.time }}</div>
          </div>
        </li>
        <li v-if="!recentHistory.length" class="muted">この期間の発信はありません</li>
      </ul>
      <p class="muted note">
        位置はブラウザの GPS（本人が許可した場合のみ）。履歴はサーバの再起動で消えます（録音の index.csv に緯度・経度が残ります）。
      </p>
    </aside>
  </div>
</template>

<style scoped>
.map-page {
  display: grid;
  grid-template-columns: minmax(0, 1fr) 340px;
  gap: 20px;
  align-items: start;
}
@media (max-width: 800px) {
  .map-page { grid-template-columns: 1fr; height: auto; }
}
.map-wrap { position: relative; min-width: 0; display: grid; grid-template-rows: auto minmax(400px, 65vh) auto; overflow: hidden; padding: 12px; border: 1px solid var(--line); border-radius: 12px; background: var(--panel); }
.map { position: relative; grid-row: 2; min-height: 400px; border-radius: 8px; overflow: hidden; }
.toolbar {
  grid-row: 1;
  padding-bottom: 12px;
  display: flex;
  flex-wrap: wrap;
  gap: 6px;
  z-index: 2;
}
.toolbar .small { padding: 8px 12px; font-size: .85rem; }
.toolbar .active { background: var(--accent); color: #fff; }
.small-select {
  padding: 6px 8px;
  font-size: .8rem;
  border-radius: 8px;
  width: auto;
  border: 1px solid var(--line);
  background: var(--field);
  color: var(--text);
}
.legend {
  grid-row: 3;
  display: flex;
  flex-wrap: wrap;
  gap: 4px 10px;
  max-width: 100%;
  padding: 14px 4px 2px;
  border-radius: 8px;
  font-size: .75rem;
  background: var(--panel);
  z-index: 2;
}
.legend i { display: inline-block; width: 10px; height: 10px; margin-right: 4px; vertical-align: -1px; }
.legend i.round { border-radius: 50%; }

.side { overflow-y: auto; max-height: calc(65vh + 120px); padding: 20px; border: 1px solid var(--line); border-radius: 12px; background: var(--panel); }
.side h2 { font-size: 1.05rem; margin: 0 0 16px; }
.list { list-style: none; padding: 0; margin: 0 0 12px; }
.list li {
  display: flex;
  gap: 10px;
  align-items: flex-start;
  padding: 14px 12px;
  margin-bottom: 12px;
  border: 1px solid var(--line);
  cursor: pointer;
  border-radius: 6px;
}
.list li:hover, .list li.selected { background: var(--selected); }
.list li.nolocation { cursor: default; opacity: .7; }
.state { flex: none; width: 12px; height: 12px; margin-top: 4px; border-radius: 3px; }
.state.round { border-radius: 50%; }
.tag { font-size: .7rem; padding: 1px 6px; margin-left: 4px; border-radius: 4px; background: var(--warn); color: #000; }
.tag.su { background: #b18cff; }
.note { margin-top: 8px; }

/* 地図上のラベル（MapLibre の Marker は scoped の外なので :deep） */
:deep(.pin) { display: flex; flex-direction: column; align-items: center; cursor: pointer; }
:deep(.pin-label) {
  order: -1;
  margin-bottom: 4px;
  padding: 2px 8px;
  border-radius: 6px;
  font-size: .78rem;
  white-space: nowrap;
  color: #fff;
  background: rgba(20, 23, 28, .85);
  border: 2px solid #30a46c;
}
:deep(.pin-dot) {
  width: 14px;
  height: 14px;
  border-radius: 50%;
  border: 2px solid #fff;
  background: #30a46c;
  box-shadow: 0 0 4px rgba(0, 0, 0, .6);
}
:deep(.pin[data-state='talk'] .pin-dot) { background: #e5484d; animation: pulse 1s infinite; }
:deep(.pin[data-state='talk'] .pin-label) { border-color: #e5484d; }
:deep(.pin[data-state='stale'] .pin-dot) { background: #8b94a3; }
:deep(.pin[data-state='stale'] .pin-label) { border-color: #8b94a3; opacity: .8; }
:deep(.pin.selected .pin-label) { outline: 2px solid #fff; }
@keyframes pulse { 50% { box-shadow: 0 0 0 8px rgba(229, 72, 77, .25); } }
</style>
