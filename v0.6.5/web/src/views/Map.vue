<script setup>
/**
 * 管理者向けの地図（3D）。接続中の人の現在地と、位置の記録（GNSS ログ CSV）を表示する。
 *
 * - 背景: 地理院タイル（標準地図 / 航空写真）
 * - 建物: OpenFreeMap（OpenMapTiles）のベクタタイルを高さ付きで立体表示
 * - 地形: Terrarium 形式の標高タイル
 * - 人: 発言中は赤、受信中は緑の柱（高さで状態が分かる）とラベル
 * - 記録: 点が集まっているところは件数の丸にまとめる。丸を押すとそこへ寄る（クローズアップ）
 * - 点・人を押すと [時間・ユーザー・場所（住所＋緯度経度）] を表示
 */
import { ref, reactive, computed, onMounted, onBeforeUnmount, shallowRef } from 'vue'
import maplibregl from 'maplibre-gl'
import 'maplibre-gl/dist/maplibre-gl.css'
import { api } from '../api.js'
import UiIcon from '../components/UiIcon.vue'

const POLL_MS = 3000 // 接続中の人の位置
const HISTORY_POLL_MS = 30000 // 記録（CSV から読むので間隔を空ける）
const STALE_MS = 2 * 60 * 1000 // これより古い位置は灰色
const LABEL_MIN_ZOOM = 13 // これより引いた表示では名前のラベルを隠す（重なり防止）
const TOKYO = [139.767, 35.681]

const mapEl = ref(null)
const map = shallowRef(null)
const error = ref('')
const enabled = ref(true)
const users = ref([])
const history = ref([])
const dates = ref([])
const geocodeOn = ref(false)
const historyMax = ref(5000)
const serverNow = ref(Date.now())
const opt = reactive({ threeD: true, photo: false, terrain: true, historyHours: 24, buildings: true, showPos: true })
const selected = ref(null)
const csvDate = ref('')

const markers = new Map() // id -> maplibregl.Marker
const addressCache = new Map() // "lat,lon" -> Promise<string|null>
let timer = null
let historyTimer = null
let fitted = false
let mapCfg = null
let ready = false // スタイルの読み込みが終わった（isStyleLoaded はタイル読み込み中も false になるため使わない）
let popup = null

const KIND = {
  POS: { label: '位置', color: '#7c8aa5' },
  PTT: { label: '発信', color: '#f5a524' },
  TTS: { label: '読み上げ', color: '#3d8bfd' },
}
const COLORS = { talk: '#e5484d', listen: '#30a46c', stale: '#8b94a3', none: '#8b94a3' }

const located = computed(() => users.value.filter((u) => u.location))
const shownHistory = computed(() => (opt.showPos ? history.value : history.value.filter((h) => h.kind !== 'POS')))
const recentHistory = computed(() => [...shownHistory.value].reverse().slice(0, 50))

function ageText(ts) {
  const s = Math.max(0, Math.round((serverNow.value - ts) / 1000))
  if (s < 60) return `${s}秒前`
  if (s < 3600) return `${Math.floor(s / 60)}分前`
  return `${Math.floor(s / 3600)}時間前`
}

function timeText(ts) {
  const d = new Date(ts)
  const p = (n) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())} ${p(d.getHours())}:${p(d.getMinutes())}:${p(d.getSeconds())}`
}

function stateOf(u) {
  if (!u.location) return 'none'
  if (serverNow.value - u.location.ts > STALE_MS) return 'stale'
  return u.talking ? 'talk' : 'listen'
}

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
    // 点が集まっているところは件数の丸にまとめる
    history: { type: 'geojson', data: empty(), cluster: true, clusterRadius: 48, clusterMaxZoom: 16 },
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
      filter: ['!', ['has', 'point_count']],
      paint: {
        'circle-radius': ['interpolate', ['linear'], ['zoom'], 8, ['match', ['get', 'kind'], 'POS', 3, 4], 16, ['match', ['get', 'kind'], 'POS', 7, 9]],
        'circle-color': ['match', ['get', 'kind'], 'TTS', KIND.TTS.color, 'PTT', KIND.PTT.color, KIND.POS.color],
        'circle-opacity': ['get', 'opacity'],
        'circle-stroke-color': '#fff',
        'circle-stroke-width': 1,
        'circle-stroke-opacity': ['get', 'opacity'],
        'circle-pitch-alignment': 'map',
      },
    },
    {
      id: 'clusters',
      type: 'circle',
      source: 'history',
      filter: ['has', 'point_count'],
      paint: {
        'circle-color': ['step', ['get', 'point_count'], '#5b8def', 20, '#f5a524', 100, '#e5484d'],
        'circle-radius': ['step', ['get', 'point_count'], 15, 20, 20, 100, 26],
        'circle-opacity': 0.85,
        'circle-stroke-color': '#fff',
        'circle-stroke-width': 2,
      },
    },
  ]
  if (m.glyphs) {
    layers.push({
      id: 'cluster-count',
      type: 'symbol',
      source: 'history',
      filter: ['has', 'point_count'],
      layout: { 'text-field': ['get', 'point_count_abbreviated'], 'text-font': ['Noto Sans Regular'], 'text-size': 13, 'text-allow-overlap': true },
      paint: { 'text-color': '#fff' },
    })
  }
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
  const style = { version: 8, sources, layers }
  if (m.glyphs) style.glyphs = m.glyphs
  return style
}

function empty() {
  return { type: 'FeatureCollection', features: [] }
}

/* ---------------- 押した点の情報（時間・ユーザー・場所） ---------------- */

function addressOf(lat, lon) {
  const key = `${lat.toFixed(4)},${lon.toFixed(4)}`
  if (!addressCache.has(key)) {
    addressCache.set(
      key,
      api
        .get(`/api/admin/geocode?lat=${lat}&lon=${lon}`)
        .then((r) => r.address)
        .catch(() => {
          addressCache.delete(key) // 失敗は次に押したときにやり直す
          return null
        }),
    )
  }
  return addressCache.get(key)
}

/** info: { ts, name, id, kind?, channel?, location } */
function showInfo(info) {
  const m = map.value
  if (!m) return
  const { lat, lon, accuracy, alt } = info.location
  const el = document.createElement('div')
  el.className = 'info-popup'
  const row = (label) => {
    const d = document.createElement('div')
    d.className = 'info-row'
    const l = document.createElement('span')
    l.className = 'info-label'
    l.textContent = label
    const v = document.createElement('span')
    v.className = 'info-value'
    d.append(l, v)
    el.append(d)
    return v
  }
  const kind = info.kind ? `（${KIND[info.kind]?.label ?? info.kind}${info.channel ? ' ' + info.channel : ''}）` : ''
  row('時間').textContent = timeText(info.ts) + kind
  row('ユーザー').textContent = `${info.name} (${info.id})`
  const place = row('場所')
  const address = document.createElement('div')
  const coords = document.createElement('div')
  coords.className = 'info-coords'
  coords.textContent =
    `${lat.toFixed(6)}, ${lon.toFixed(6)}` + (accuracy ? `（±${Math.round(accuracy)}m）` : '') + (alt != null ? ` 高度 ${Math.round(alt)}m` : '')
  place.append(address, coords)
  if (geocodeOn.value) {
    address.textContent = '住所を取得中…'
    addressOf(lat, lon).then((a) => (address.textContent = a || '住所なし（海上・国外など）'))
  }
  popup?.remove()
  popup = new maplibregl.Popup({ offset: 14, maxWidth: '320px' }).setLngLat([lon, lat]).setDOMContent(el).addTo(m)
}

/* ---------------- 地図の更新 ---------------- */

function markerEl(u) {
  const el = document.createElement('div')
  el.className = 'pin'
  el.innerHTML = '<span class="pin-dot"></span><span class="pin-label"></span>'
  el.addEventListener('click', (e) => {
    e.stopPropagation()
    focusUser(u.id, true)
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
    pillars.push({
      type: 'Feature',
      properties: { color, height: st === 'talk' ? 120 : st === 'listen' ? 45 : 15 },
      geometry: { type: 'Polygon', coordinates: circle(lon, lat, 4, 16) },
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
  m.getSource('pillars')?.setData({ type: 'FeatureCollection', features: pillars })
  m.getSource('accuracy')?.setData({ type: 'FeatureCollection', features: acc })
  if (!fitted && located.value.length) {
    fitted = true
    fitAll(false)
  }
}

/** 記録の点（CSV から読んだもの）。読み込み・表示切り替えのときだけ作り直す */
function renderHistory() {
  const m = map.value
  if (!m || !ready) return
  const span = Math.max(1, opt.historyHours > 0 ? opt.historyHours * 3600 * 1000 : 24 * 3600 * 1000)
  const features = shownHistory.value.map((h, i) => ({
    type: 'Feature',
    properties: { i, kind: h.kind, opacity: Math.max(0.35, 1 - (serverNow.value - h.ts) / span) },
    geometry: { type: 'Point', coordinates: [h.location.lon, h.location.lat] },
  }))
  m.getSource('history')?.setData({ type: 'FeatureCollection', features })
}

function fitAll(animate = true) {
  const pts = located.value.map((u) => [u.location.lon, u.location.lat])
  if (!pts.length) shownHistory.value.forEach((h) => pts.push([h.location.lon, h.location.lat]))
  if (!map.value || !pts.length) return
  fitPoints(pts, animate, 16)
}

function fitPoints(pts, animate = true, single = 17) {
  const m = map.value
  if (pts.length === 1) {
    m.easeTo({ center: pts[0], zoom: single, duration: animate ? 800 : 0 })
    return
  }
  const b = new maplibregl.LngLatBounds(pts[0], pts[0])
  pts.forEach((p) => b.extend(p))
  m.fitBounds(b, { padding: { top: 70, bottom: 50, left: 60, right: 60 }, maxZoom: 17, duration: animate ? 800 : 0 })
}

/** 点（人・記録）が最も集まっている場所へ寄る（約 300m 四方のマスと周りのマスで数える） */
function zoomToDensest() {
  const pts = [...located.value.map((u) => [u.location.lon, u.location.lat]), ...shownHistory.value.map((h) => [h.location.lon, h.location.lat])]
  if (!pts.length || !map.value) return
  const cell = 0.003
  const cells = new Map()
  for (const p of pts) {
    const key = `${Math.floor(p[0] / cell)},${Math.floor(p[1] / cell)}`
    if (!cells.has(key)) cells.set(key, [])
    cells.get(key).push(p)
  }
  let best = null
  for (const key of cells.keys()) {
    const [x, y] = key.split(',').map(Number)
    const group = []
    for (let dx = -1; dx <= 1; dx++) for (let dy = -1; dy <= 1; dy++) group.push(...(cells.get(`${x + dx},${y + dy}`) || []))
    if (!best || group.length > best.length) best = group
  }
  fitPoints(best)
}

function focusUser(id, info = false) {
  selected.value = id
  const u = users.value.find((x) => x.id === id)
  if (u?.location) {
    flyTo(u.location)
    if (info) showInfo({ ts: u.location.ts, name: u.name, id: u.display_id ?? u.id, location: u.location })
  }
  render()
}

function focusHistory(h) {
  flyTo(h.location)
  showInfo(h)
}

function flyTo(loc) {
  const m = map.value
  m?.flyTo({ center: [loc.lon, loc.lat], zoom: Math.max(17, m.getZoom()), pitch: opt.threeD ? 60 : 0, duration: 1200 })
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
  renderHistory()
}

function toggle3d() {
  opt.threeD = !opt.threeD
  map.value?.easeTo({ pitch: opt.threeD ? 55 : 0, bearing: opt.threeD ? map.value.getBearing() : 0, duration: 600 })
  applyOptions()
}

/* ---------------- データ ---------------- */

async function load(withHistory = false) {
  try {
    const r = await api.get(`/api/admin/locations?hours=${withHistory ? opt.historyHours : 0}`)
    enabled.value = r.enabled
    users.value = r.users
    serverNow.value = r.now
    geocodeOn.value = !!r.geocode
    dates.value = r.dates || []
    historyMax.value = r.max_points || historyMax.value
    if (!dates.value.includes(csvDate.value)) csvDate.value = dates.value[0] || ''
    if (withHistory) history.value = r.history
    if (!mapCfg) {
      mapCfg = r.map
      initMap()
    }
    error.value = ''
    render()
    if (withHistory) renderHistory()
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
  const labels = () => mapEl.value?.classList.toggle('hide-labels', m.getZoom() < LABEL_MIN_ZOOM)
  m.on('zoomend', labels)
  m.on('load', () => {
    ready = true
    labels()
    applyOptions()
    if (!fitted && shownHistory.value.length) {
      fitted = true
      fitAll(false)
    }
  })
  // まとまり（件数の丸）を押したら、ばらける所までズーム（クローズアップ）
  m.on('click', 'clusters', async (e) => {
    const f = e.features?.[0]
    if (!f) return
    let zoom = m.getZoom() + 2
    try {
      zoom = (await m.getSource('history').getClusterExpansionZoom(f.properties.cluster_id)) + 0.5
    } catch {
      /* 取れなければ 2 段階寄る */
    }
    m.easeTo({ center: f.geometry.coordinates, zoom: Math.min(zoom, 19), duration: 700 })
  })
  m.on('click', 'history', (e) => {
    const f = e.features?.[0]
    const h = f && shownHistory.value[f.properties.i]
    if (h) showInfo(h)
  })
  for (const layer of ['clusters', 'history']) {
    m.on('mouseenter', layer, () => (m.getCanvas().style.cursor = 'pointer'))
    m.on('mouseleave', layer, () => (m.getCanvas().style.cursor = ''))
  }
  m.on('click', (e) => {
    if (m.queryRenderedFeatures(e.point, { layers: ['clusters', 'history'] }).length) return
    selected.value = null
    render()
  })
  // タイルが取れないなどのエラーは地図の表示を止めない
  m.on('error', (e) => console.warn('map:', e?.error?.message || e))
  map.value = m
  mapEl.value.__map = m // 動作確認用（ブラウザの開発ツールから触れるように）
}

onMounted(() => {
  load(true)
  timer = setInterval(() => load(false), POLL_MS)
  historyTimer = setInterval(() => load(true), HISTORY_POLL_MS)
})

onBeforeUnmount(() => {
  clearInterval(timer)
  clearInterval(historyTimer)
  popup?.remove()
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
        <button class="secondary small" title="点が最も集まっている場所へ寄る" @click="zoomToDensest"><UiIcon name="map" />密集地点へ</button>
        <button class="secondary small" :class="{ active: opt.threeD }" @click="toggle3d">{{ opt.threeD ? '3D' : '2D' }}</button>
        <button class="secondary small" :class="{ active: opt.photo }" @click="(opt.photo = !opt.photo), applyOptions()">航空写真</button>
        <button class="secondary small" :class="{ active: opt.terrain }" :disabled="!opt.threeD" @click="(opt.terrain = !opt.terrain), applyOptions()">地形</button>
        <button class="secondary small" :class="{ active: opt.buildings }" :disabled="!opt.threeD" @click="(opt.buildings = !opt.buildings), applyOptions()">建物</button>
        <button class="secondary small" :class="{ active: opt.showPos }" title="定期的に記録した位置も表示する" @click="(opt.showPos = !opt.showPos), renderHistory()">移動の記録</button>
        <select v-model.number="opt.historyHours" class="small-select" aria-label="記録の表示期間" @change="load(true)">
          <option :value="0">記録: 表示しない</option>
          <option :value="1">記録: 1時間</option>
          <option :value="24">記録: 24時間</option>
          <option :value="168">記録: 7日</option>
        </select>
      </div>
      <div class="legend">
        <span><i style="background: #e5484d"></i>発言中</span>
        <span><i style="background: #30a46c"></i>受信中</span>
        <span><i style="background: #8b94a3"></i>2分以上更新なし</span>
        <span><i class="round" :style="{ background: KIND.POS.color }"></i>位置の記録</span>
        <span><i class="round" :style="{ background: KIND.PTT.color }"></i>発信した場所</span>
        <span><i class="round" :style="{ background: KIND.TTS.color }"></i>読み上げした場所</span>
        <span><i class="round" style="background: #5b8def"></i>数字の丸 = 集まっている点（押すと寄る）</span>
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
          @click="u.location && focusUser(u.id, true)"
          @keydown.enter="u.location && focusUser(u.id, true)"
          @keydown.space.prevent="u.location && focusUser(u.id, true)"
        >
          <span class="state" :style="{ background: COLORS[stateOf(u)] }"></span>
          <div class="grow">
            <div>
              {{ u.name }} <span class="muted">({{ u.display_id ?? u.id }})</span>
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

      <h2>記録（新しい順・最大50件）</h2>
      <ul class="list history">
        <li v-for="h in recentHistory" :key="h.ts + h.id + h.kind" tabindex="0" role="button" @click="focusHistory(h)" @keydown.enter="focusHistory(h)" @keydown.space.prevent="focusHistory(h)">
          <span class="state round" :style="{ background: KIND[h.kind]?.color }"></span>
          <div class="grow">
            <div>{{ h.name }} <span class="muted">({{ h.id }}) {{ KIND[h.kind]?.label }} {{ h.channel }}</span></div>
            <div class="muted">{{ h.time }}</div>
          </div>
        </li>
        <li v-if="!recentHistory.length" class="muted">この期間の記録はありません</li>
      </ul>
      <p v-if="history.length >= historyMax" class="muted note">点が多いため、位置の記録を間引いて表示しています（発信・読み上げは全件）。</p>

      <h2>CSV（GNSS ログ）</h2>
      <div class="csv-row">
        <select v-model="csvDate" class="small-select" aria-label="ダウンロードする日付" :disabled="!dates.length">
          <option v-if="!dates.length" value="">記録なし</option>
          <option v-for="d in dates" :key="d" :value="d">{{ d }}</option>
        </select>
        <a class="csv-button" :class="{ disabled: !csvDate }" :href="csvDate ? `/api/admin/gnss/${csvDate}.csv` : undefined" download><UiIcon name="download" />ダウンロード</a>
      </div>
      <p class="muted note">
        位置はブラウザの GPS（本人が許可した場合のみ）。地図の記録はサーバの <code>data/gnss/日付.csv</code> から表示しています（サーバを再起動しても消えません）。
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
.csv-row { display: flex; gap: 8px; align-items: center; margin-bottom: 8px; }
.csv-row select { flex: 1; min-height: 40px; }
.csv-button { display: inline-flex; align-items: center; gap: 6px; min-height: 40px; padding: 8px 12px; font-size: .85rem; text-decoration: none; border-radius: 8px; border: 1px solid var(--line); color: var(--text); background: var(--field); white-space: nowrap; }
.csv-button:hover { background: var(--soft); }
.csv-button.disabled { pointer-events: none; opacity: .5; }

/* スマホ: 地図を大きく、ボタンは横にスクロール */
@media (max-width: 760px) {
  .map-wrap { padding: 8px; grid-template-rows: auto minmax(360px, 60vh) auto; }
  .map { min-height: 360px; }
  .toolbar { flex-wrap: nowrap; overflow-x: auto; padding-bottom: 8px; -webkit-overflow-scrolling: touch; }
  .toolbar > * { flex: 0 0 auto; }
  .side { max-height: none; padding: 16px; }
}

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
.map.hide-labels :deep(.pin:not(.selected) .pin-label) { display: none; }
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

/* 押した点の情報（地図の上なのでテーマに関係なく白地） */
:deep(.maplibregl-popup-content) { padding: 12px 30px 12px 14px; border-radius: 10px; color: #1b2430; font-size: .85rem; }
:deep(.maplibregl-popup-close-button) { width: 30px; height: 30px; min-height: 0; padding: 0; font-size: 1.3rem; line-height: 1; color: #1b2430; background: transparent; border: 0; border-radius: 6px; }
:deep(.maplibregl-popup-close-button:hover) { background: #eef1f5; }
:deep(.info-popup) { display: grid; gap: 6px; }
:deep(.info-row) { display: grid; grid-template-columns: 4.5em 1fr; gap: 8px; }
:deep(.info-label) { color: #5d6b7e; font-weight: 700; }
:deep(.info-value) { overflow-wrap: anywhere; }
:deep(.info-coords) { color: #5d6b7e; font-size: .78rem; margin-top: 2px; }
</style>
