<script setup>
import { ref, reactive, computed, onBeforeUnmount, onMounted } from 'vue'
import { api, session } from '../api.js'
import UiIcon from '../components/UiIcon.vue'
import ChannelBadge from '../components/ChannelBadge.vue'

const SIDES = ['L', 'R']
const status = ref('idle') // idle | connecting | connected
const message = ref('')
const online = ref(0)
const maxConnections = ref(0)
const channels = ref([])
const lr = ref(loadPref('lr', false))
const talking = reactive({ side: null, state: 'off' }) // state: off | requesting | on
const cfg = reactive({ ice_servers: [], tts: false, tts_max_chars: 200, location: false })
const voice = ref(null)
const ttsText = ref('')
const ttsSide = ref('L')
const ttsLog = ref([])
const panningOk = ref(true)
/** 位置情報: off | waiting | on | denied | unsupported */
const geo = ref('off')

/** 受聴口ごとの状態（pc 等の非リアクティブなものは legs に持つ） */
const sides = reactive({
  L: { channel: loadPref('chL', null), state: 'off', volume: 1 },
  R: { channel: loadPref('chR', null), state: 'off', volume: 1 },
})
const legs = { L: {}, R: {} }
const audioEls = { L: ref(null), R: ref(null) }
const audioElL = audioEls.L
const audioElR = audioEls.R

let ws = null
let micStream = null
let audioCtx = null
let wakeLock = null
let pressed = null // 押している受聴口
let geoWatch = null
let lastGeo = null // 最後に送った位置 { lat, lon, t }

const activeSides = computed(() => (lr.value ? SIDES : ['L']))
const channelById = (id) => channels.value.find((c) => c.id === id)

function loadPref(key, def) {
  try {
    const v = localStorage.getItem('macross.' + key)
    return v === null ? def : JSON.parse(v)
  } catch {
    return def
  }
}
function savePref(key, v) {
  try {
    localStorage.setItem('macross.' + key, JSON.stringify(v))
  } catch {
    /* 保存できなくても動作には影響しない */
  }
}

function sideLabel(side) {
  if (!lr.value) return '受信'
  return side === 'L' ? '左 (L)' : '右 (R)'
}

function rawFull(side) {
  const ch = channelById(sides[side].channel)
  if (!ch || talking.side === side) return false
  return ch.speakers.length >= ch.max_speakers
}

/** 割り込みの強さ（スーパーユーザー 2 / 管理者 1 / 一般 0） */
const rank = (u) => (u?.superuser ? 2 : u?.admin ? 1 : 0)

/** 上位のロールは下位の人の発言を止めて話せる */
function canPreempt(side) {
  const ch = channelById(sides[side].channel)
  const me = rank(session.user)
  return me > 0 && rawFull(side) && ch.speakers.some((s) => rank(s) < me)
}

/** 発言者の表示行数（発言枠ぶん確保して、話し始めても表示がずれないようにする。最大 3 行） */
function speakerRows(side) {
  const ch = channelById(sides[side].channel)
  return Math.min(Math.max(ch?.max_speakers ?? 1, 1), 3)
}

function slotsFull(side) {
  return rawFull(side) && !canPreempt(side)
}

function beep(freq, ms = 70) {
  if (!audioCtx) return
  const osc = audioCtx.createOscillator()
  const gain = audioCtx.createGain()
  osc.frequency.value = freq
  gain.gain.value = 0.08
  osc.connect(gain).connect(audioCtx.destination)
  osc.start()
  osc.stop(audioCtx.currentTime + ms / 1000)
}

function waitIceGathering(peer, timeoutMs = 3000) {
  if (peer.iceGatheringState === 'complete') return Promise.resolve()
  return new Promise((resolve) => {
    const done = () => {
      peer.removeEventListener('icegatheringstatechange', check)
      resolve()
    }
    const check = () => peer.iceGatheringState === 'complete' && done()
    peer.addEventListener('icegatheringstatechange', check)
    setTimeout(done, timeoutMs)
  })
}

/* ---------------- 音声出力（LR のときは Web Audio で左右に振る） ---------------- */

function ensureNodes(side) {
  const leg = legs[side]
  if (leg.gain || !audioCtx || !leg.stream) return
  try {
    leg.source = audioCtx.createMediaStreamSource(leg.stream)
    leg.gain = audioCtx.createGain()
    leg.panner = audioCtx.createStereoPanner ? audioCtx.createStereoPanner() : null
    leg.source.connect(leg.gain)
    if (leg.panner) leg.gain.connect(leg.panner).connect(audioCtx.destination)
    else leg.gain.connect(audioCtx.destination)
    if (!leg.panner) panningOk.value = false
  } catch {
    panningOk.value = false
    leg.gain = null
  }
}

/** LR なら Web Audio（左右に定位）、そうでなければ audio 要素で再生（エコーキャンセルが効きやすい） */
function applyOutput(side) {
  const leg = legs[side]
  const el = audioEls[side].value
  if (!el || !leg.stream) return
  ensureNodes(side)
  const useGraph = lr.value && !!leg.gain
  el.muted = useGraph
  el.volume = sides[side].volume
  if (leg.gain) leg.gain.gain.value = useGraph ? sides[side].volume : 0
  if (leg.panner) leg.panner.pan.value = side === 'L' ? -1 : 1
  el.play().catch(() => {})
}

/* ---------------- 位置情報（GPS） ---------------- */

const GEO_MIN_MS = 5000 // 送信間隔の下限
const GEO_MOVE_M = 10 // これ以上動いたら間隔に関係なく送る（ただし 1 秒に 1 回まで）

function distanceM(a, b) {
  const R = 6371000
  const rad = Math.PI / 180
  const dLat = (b.lat - a.lat) * rad
  const dLon = (b.lon - a.lon) * rad
  const h = Math.sin(dLat / 2) ** 2 + Math.cos(a.lat * rad) * Math.cos(b.lat * rad) * Math.sin(dLon / 2) ** 2
  return 2 * R * Math.asin(Math.sqrt(h))
}

function startGeo() {
  if (!cfg.location) return
  if (!navigator.geolocation) {
    geo.value = 'unsupported'
    return
  }
  geo.value = 'waiting'
  geoWatch = navigator.geolocation.watchPosition(
    (pos) => {
      const c = pos.coords
      const now = Date.now()
      const cur = { lat: c.latitude, lon: c.longitude }
      geo.value = 'on'
      if (lastGeo) {
        const dt = now - lastGeo.t
        if (dt < 1000 || (dt < GEO_MIN_MS && distanceM(lastGeo, cur) < GEO_MOVE_M)) return
      }
      lastGeo = { ...cur, t: now }
      send({ type: 'location', lat: c.latitude, lon: c.longitude, alt: c.altitude, accuracy: c.accuracy })
    },
    (err) => {
      geo.value = err.code === err.PERMISSION_DENIED ? 'denied' : geo.value === 'on' ? 'on' : 'waiting'
    },
    { enableHighAccuracy: true, maximumAge: 5000, timeout: 30000 },
  )
}

function stopGeo() {
  if (geoWatch !== null) navigator.geolocation?.clearWatch(geoWatch)
  geoWatch = null
  lastGeo = null
  geo.value = 'off'
}

/* ---------------- 接続 ---------------- */

async function connect() {
  message.value = ''
  status.value = 'connecting'
  try {
    // iOS Safari の自動再生制限のため、クリック直後に AudioContext を作る
    audioCtx = new (window.AudioContext || window.webkitAudioContext)()
    audioCtx.resume?.()
    for (const s of SIDES) audioEls[s].value?.play().catch(() => {})

    micStream = await navigator.mediaDevices.getUserMedia({
      audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true },
      video: false,
    })
    Object.assign(cfg, await api.get('/api/config'))
    voice.value = await api.get('/api/tts/voice').catch(() => null)

    const proto = location.protocol === 'https:' ? 'wss' : 'ws'
    ws = new WebSocket(`${proto}://${location.host}/ws`)
    ws.onmessage = (e) => onMessage(JSON.parse(e.data))
    ws.onclose = () => {
      if (status.value !== 'idle') disconnect('サーバとの接続が切れました')
    }
    try {
      wakeLock = await navigator.wakeLock?.request('screen')
    } catch {
      /* 画面スリープ防止は任意 */
    }
  } catch (e) {
    disconnect(e.name === 'NotAllowedError' ? 'マイクの使用が許可されていません' : e.message)
  }
}

/** 最初の state を受け取ったら、前回のチャンネル（無ければ先頭）に参加する */
function autoJoin() {
  for (const side of activeSides.value) {
    const other = sides[side === 'L' ? 'R' : 'L'].channel
    let id = sides[side].channel
    if (!channelById(id) || (side === 'R' && id === other)) {
      id = channels.value.find((c) => side === 'L' || c.id !== other)?.id ?? null
    }
    if (id !== null) joinSide(side, id)
  }
}

async function joinSide(side, channelId) {
  closeLeg(side)
  sides[side].channel = channelId
  savePref('ch' + side, channelId)
  if (channelId === null || status.value !== 'connected') return
  sides[side].state = 'joining'
  try {
    const track = micStream.getAudioTracks()[0].clone()
    track.enabled = false
    const pc = new RTCPeerConnection({ iceServers: cfg.ice_servers })
    const stream = new MediaStream()
    legs[side] = { pc, track, stream }
    pc.addTrack(track, micStream)
    pc.ontrack = (e) => {
      stream.addTrack(e.track)
      audioEls[side].value.srcObject = stream
      applyOutput(side)
    }
    pc.onconnectionstatechange = () => {
      if (legs[side].pc === pc && ['failed', 'closed'].includes(pc.connectionState)) {
        closeLeg(side)
        message.value = `${sideLabel(side)} の音声接続が切れました`
      }
    }
    await pc.setLocalDescription(await pc.createOffer())
    await waitIceGathering(pc)
    if (legs[side].pc !== pc) return // 待っている間に切り替えられた
    send({ type: 'join', side, channel: channelId, sdp: pc.localDescription.sdp })
  } catch (e) {
    closeLeg(side)
    message.value = e.message
  }
}

/** 受聴口をブラウザ側で閉じる（サーバへの leave は送らない） */
function closeLeg(side) {
  const leg = legs[side]
  if (talking.side === side) pttUp()
  leg.pc?.close()
  leg.track?.stop()
  leg.source?.disconnect()
  leg.gain?.disconnect()
  leg.panner?.disconnect()
  legs[side] = {}
  sides[side].state = 'off'
  const el = audioEls[side].value
  if (el) el.srcObject = null
}

function leaveSide(side) {
  if (sides[side].state !== 'off') send({ type: 'leave', side })
  closeLeg(side)
}

function onSelect(side, value) {
  const id = value === '' ? null : Number(value)
  if (sides[side].state !== 'off') send({ type: 'leave', side })
  joinSide(side, id)
}

function toggleLr() {
  lr.value = !lr.value
  savePref('lr', lr.value)
  if (lr.value) {
    const other = sides.L.channel
    let id = sides.R.channel
    if (!channelById(id) || id === other) id = channels.value.find((c) => c.id !== other)?.id ?? null
    if (status.value === 'connected' && id !== null) joinSide('R', id)
  } else {
    leaveSide('R')
    ttsSide.value = 'L'
  }
  applyOutput('L')
}

function setVolume(side, v) {
  sides[side].volume = Number(v)
  applyOutput(side)
}

/* ---------------- サーバからのメッセージ ---------------- */

async function onMessage(msg) {
  switch (msg.type) {
    case 'state': {
      const first = status.value === 'connecting'
      online.value = msg.online
      maxConnections.value = msg.max_connections
      channels.value = msg.channels
      if (first) {
        status.value = 'connected'
        autoJoin()
        startGeo()
      } else {
        // 使えるチャンネルが変わった（管理者が権限を変更した・チャンネルが削除された）
        for (const side of SIDES) {
          if (sides[side].state === 'off' && sides[side].channel !== null && !channelById(sides[side].channel)) {
            sides[side].channel = null
          }
        }
      }
      break
    }
    case 'answer': {
      const leg = legs[msg.side]
      if (!leg.pc || sides[msg.side].state !== 'joining') break
      await leg.pc.setRemoteDescription({ type: 'answer', sdp: msg.sdp })
      sides[msg.side].state = 'on'
      break
    }
    case 'left':
      closeLeg(msg.side)
      message.value = `${sideLabel(msg.side)}: ${msg.reason}`
      break
    case 'ptt':
      if (msg.granted) {
        if (pressed === msg.side && talking.side === msg.side) {
          legs[msg.side].track && (legs[msg.side].track.enabled = true)
          talking.state = 'on'
          beep(880)
        } else {
          send({ type: 'ptt', side: msg.side, on: false })
        }
      } else if (talking.side === msg.side) {
        if (msg.reason) {
          message.value = msg.reason
          beep(220, 150)
        }
        stopTalkingLocal()
      }
      break
    case 'tts': {
      const ch = channelById(msg.channel)
      ttsLog.value.unshift({ ...msg, key: Math.random(), channelName: ch?.name ?? '' })
      ttsLog.value.splice(30)
      break
    }
    case 'error':
      message.value = msg.message
      if (status.value === 'connecting') disconnect(msg.message)
      break
    case 'hangup':
      disconnect(msg.reason)
      break
  }
}

function send(obj) {
  if (ws && ws.readyState === WebSocket.OPEN) ws.send(JSON.stringify(obj))
}

/* ---------------- PTT ---------------- */

function pttDown(side, e) {
  if (sides[side].state !== 'on' || pressed) return
  e?.preventDefault?.()
  if (e?.pointerId !== undefined) e.currentTarget.setPointerCapture(e.pointerId)
  pressed = side
  message.value = ''
  talking.side = side
  talking.state = 'requesting'
  send({ type: 'ptt', side, on: true })
}

function stopTalkingLocal() {
  for (const s of SIDES) if (legs[s].track) legs[s].track.enabled = false
  talking.side = null
  talking.state = 'off'
}

function pttUp() {
  if (!pressed) return
  const side = pressed
  pressed = null
  if (talking.state === 'on') beep(440)
  if (talking.state !== 'off') send({ type: 'ptt', side, on: false })
  stopTalkingLocal()
}

function onKey(e) {
  const side = e.code === 'Space' ? 'L' : e.code === 'Enter' && lr.value ? 'R' : null
  if (!side) return
  if (e.type === 'keyup' && pressed === side) {
    e.preventDefault()
    pttUp()
    return
  }
  if (['INPUT', 'TEXTAREA', 'SELECT'].includes(e.target.tagName) || e.target.closest('button:not(.ptt), a, summary, [contenteditable="true"]')) return
  e.preventDefault()
  if (e.type === 'keydown' && !e.repeat) pttDown(side)
}

/* ---------------- 読み上げ ---------------- */

function sendTts() {
  const text = ttsText.value.trim()
  if (!text) return
  if (sides[ttsSide.value].state !== 'on') {
    message.value = '読み上げ先のチャンネルに参加していません'
    return
  }
  message.value = ''
  send({ type: 'tts', side: ttsSide.value, text })
  ttsText.value = ''
}

function disconnect(reason = '') {
  stopGeo()
  pressed = null
  stopTalkingLocal()
  for (const s of SIDES) closeLeg(s)
  status.value = 'idle'
  channels.value = []
  if (reason) message.value = reason
  if (ws) {
    ws.onclose = null
    ws.close()
    ws = null
  }
  micStream?.getTracks().forEach((t) => t.stop())
  micStream = null
  audioCtx?.close()
  audioCtx = null
  wakeLock?.release?.()
  wakeLock = null
}

onMounted(() => {
  window.addEventListener('keydown', onKey)
  window.addEventListener('keyup', onKey)
})
onBeforeUnmount(() => {
  window.removeEventListener('keydown', onKey)
  window.removeEventListener('keyup', onKey)
  disconnect()
})
</script>

<template>
  <div class="container radio-page">
    <section class="radio-toolbar panel" aria-label="無線の接続と受聴モード">
      <div class="row connection-controls">
        <span class="connection-status" :class="status"><i aria-hidden="true"></i>{{ { idle: '未接続', connecting: '接続中…', connected: '接続済み' }[status] }}</span>
        <span v-if="status === 'connected'" class="connection-count">{{ online }} / {{ maxConnections }} 人</span>
        <button v-if="status !== 'connected'" :disabled="status === 'connecting'" @click="connect">{{ status === 'connecting' ? '接続中…' : '接続する' }}</button>
        <button v-else class="secondary" @click="disconnect()">切断する</button>
      </div>
      <div class="row mode-controls">
        <div class="segmented" role="group" aria-label="受聴モード">
          <button type="button" :class="{ selected: !lr }" :aria-pressed="!lr" @click="lr && toggleLr()">通常</button>
          <button type="button" :class="{ selected: lr }" :aria-pressed="lr" @click="!lr && toggleLr()">LR受聴</button>
        </div>
        <span class="muted mode-hint"><UiIcon name="headphones" /> {{ lr ? '左右の耳で別のチャンネルを受聴' : 'チャンネルを選んで、押して話す' }}</span>
        <span v-if="status === 'connected' && cfg.location" class="geo" :class="{ shared: geo === 'on' }"><UiIcon name="map" />位置情報：{{ { off: '—', waiting: '取得中…', on: '管理者に共有中', denied: '許可されていません', unsupported: 'このブラウザは未対応' }[geo] }}</span>
      </div>
    </section>
    <p v-if="lr && !panningOk" class="error" role="alert">このブラウザは左右の振り分けに対応していないため、両方とも中央で聞こえます。</p>
    <p v-if="message" class="error" role="alert">{{ message }}</p>
    <div class="radio-layout" :class="{ 'with-tts': status === 'connected' }">
      <section class="radio-main" aria-label="受信・送信チャンネル">
        <div class="sides" :class="{ two: lr }">
          <div v-for="side in activeSides" :key="side" class="panel side">
            <div class="side-heading"><span v-if="lr" class="side-letter">{{ side }}</span><h2>{{ lr ? side === 'L' ? '左チャンネル' : '右チャンネル' : '受信チャンネル' }}</h2><span class="status-chip" :class="{ success: sides[side].state === 'on', warning: sides[side].state === 'joining' }">{{ { off: '未参加', joining: '参加中…', on: '参加中' }[sides[side].state] }}</span></div>
            <div class="channel-select">
              <ChannelBadge :id="sides[side].channel" />
              <select :aria-label="lr ? `${sideLabel(side)}のチャンネル` : '受信チャンネル'" :value="sides[side].channel ?? ''" :disabled="status !== 'connected'" @change="onSelect(side, $event.target.value)">
                <option value="">チャンネル未選択</option>
                <option v-for="c in channels" :key="c.id" :value="c.id" :disabled="lr && c.id === sides[side === 'L' ? 'R' : 'L'].channel">{{ c.name }}</option>
              </select>
              <div class="channel-counts"><span>{{ channelById(sides[side].channel)?.listeners ?? '—' }}人が参加</span><span>発言枠 {{ channelById(sides[side].channel)?.speakers.length ?? '—' }} / {{ channelById(sides[side].channel)?.max_speakers ?? '—' }}</span></div>
            </div>
            <ul class="speakers" :class="{ receiving: channelById(sides[side].channel)?.speakers.length }" :style="{ minHeight: speakerRows(side) * 32 + 20 + 'px' }" aria-live="polite">
              <template v-if="channelById(sides[side].channel)?.speakers.length"><li v-for="s in channelById(sides[side].channel).speakers" :key="s.id" :class="{ self: s.id === session.user?.id }"><UiIcon :name="s.id === session.user?.id ? 'mic' : 'volume'" /><span>{{ s.id === session.user?.id ? '送信中' : '受信中' }}</span><strong>{{ s.name }}</strong><span>({{ s.id }})</span><span v-if="s.superuser" class="tag su">SU</span><span v-else-if="s.admin" class="tag">管理者</span><span v-if="s.id === session.user?.id">あなた</span></li></template>
              <li v-else class="muted idle-line">{{ channelById(sides[side].channel) ? '現在発言なし' : 'チャンネル未選択' }}</li>
            </ul>
            <button class="ptt" :class="{ on: talking.side === side && talking.state === 'on', requesting: talking.side === side && talking.state === 'requesting', full: slotsFull(side), preempt: canPreempt(side) }" :disabled="sides[side].state !== 'on' || (talking.side && talking.side !== side)" :aria-pressed="talking.side === side && talking.state === 'on'" @pointerdown="pttDown(side, $event)" @pointerup="pttUp" @pointercancel="pttUp" @lostpointercapture="pttUp" @contextmenu.prevent>
              <UiIcon name="mic" /><span class="ptt-copy"><strong v-if="talking.side === side && talking.state === 'on'">送信中</strong><strong v-else-if="talking.side === side">接続待ち…</strong><strong v-else-if="canPreempt(side)">割り込んで話す</strong><strong v-else-if="slotsFull(side)">発言枠いっぱい</strong><strong v-else>押して話す</strong><small>押している間だけ送信</small></span><kbd>{{ side === 'L' ? 'Space' : 'Enter' }}</kbd>
            </button>
            <label class="volume"><span>音量</span><UiIcon name="volume" /><input type="range" min="0" max="1" step="0.05" :aria-label="`${sideLabel(side)}の音量`" :value="sides[side].volume" @input="setVolume(side, $event.target.value)" /><output>{{ Math.round(sides[side].volume * 100) }}%</output></label>
          </div>
        </div>
        <p class="transmit-note panel"><UiIcon name="mic" />ボタンを押している間だけ送信・録音されます。</p>
      </section>
      <aside v-if="status === 'connected'" class="panel tts-panel">
        <h2><UiIcon name="volume" /> 読み上げ</h2><p class="muted">テキストを音声で送信</p>
        <p v-if="!cfg.tts" class="muted">サーバで読み上げが無効になっています。</p>
        <template v-else>
          <form @submit.prevent="sendTts">
            <label for="tts-destination">送信先</label><select v-if="lr" id="tts-destination" v-model="ttsSide"><option v-for="s in SIDES" :key="s" :value="s">{{ s }}｜{{ channelById(sides[s].channel)?.name ?? '未選択' }}</option></select><div v-else class="tts-destination"><ChannelBadge :id="sides.L.channel" />{{ channelById(sides.L.channel)?.name ?? 'チャンネル未選択' }}</div>
            <label for="tts-message" class="muted">読み上げる文章</label><textarea id="tts-message" v-model="ttsText" rows="4" :maxlength="cfg.tts_max_chars" placeholder="読み上げる文章を入力" /><div class="char-count muted">{{ ttsText.length }} / {{ cfg.tts_max_chars }}</div>
            <button type="submit" class="outline tts-send" :disabled="!ttsText.trim()"><UiIcon name="volume" />読み上げで送信</button>
          </form>
          <details v-if="voice" class="voice-info"><summary><UiIcon name="info" /> 音声情報</summary><p class="muted">{{ voice.model }} / 高さ {{ voice.pitch > 0 ? '+' : '' }}{{ voice.pitch }} / 速さ ×{{ voice.speed }} / 声質 {{ voice.alpha }}</p></details>
          <h3 class="log-title">読み上げ履歴</h3><ul class="log"><li v-for="t in ttsLog" :key="t.key"><span class="muted">{{ t.time }} <span class="log-channel">{{ t.channelName }}</span> {{ t.name }}</span><p>{{ t.text }}</p></li><li v-if="!ttsLog.length" class="muted">まだ読み上げはありません。</li></ul>
        </template>
      </aside>
    </div>
    <audio ref="audioElL" autoplay playsinline></audio><audio ref="audioElR" autoplay playsinline></audio>
  </div>
</template>

<style scoped>
.radio-toolbar { padding: 16px 20px; margin-bottom: 20px; display: flex; gap: 16px; flex-wrap: wrap; align-items: center; }
.connection-controls { margin-left: auto; order: 2; }
.mode-controls { flex: 1; }
.connection-status { display: inline-flex; align-items: center; gap: 9px; font-weight: 700; white-space: nowrap; color: var(--muted); }
.connection-status i { width: 12px; height: 12px; border-radius: 50%; background: currentColor; }
.connection-status.connected, .geo.shared { color: var(--ok); }
.connection-status.connecting { color: var(--warn); }
.connection-count { white-space: nowrap; }
.geo { font-size: .85rem; }
.radio-layout { display: grid; gap: 20px; }
.radio-layout.with-tts { grid-template-columns: minmax(0, 1fr) 330px; align-items: start; }
.radio-main { min-width: 0; }
.sides { display: grid; gap: 18px; }
.sides.two { grid-template-columns: repeat(2, minmax(0, 1fr)); }
.side { display: flex; flex-direction: column; gap: 22px; }
.side-heading { display: flex; align-items: center; gap: 12px; flex-wrap: wrap; }
.side-heading h2 { margin: 0; font-size: 1.3rem; flex: 1; }
.side-heading .status-chip { font-size: .75rem; }
.side-letter { display: grid; place-items: center; width: 40px; height: 40px; border-radius: 8px; background: var(--accent); color: #fff; font-weight: 750; font-size: 1.5rem; }
.channel-select { display: flex; align-items: center; gap: 12px; flex-wrap: wrap; }
.channel-select select { flex: 1; width: 100px; font-size: 1.25rem; font-weight: 700; min-height: 62px; }
.channel-counts { width: 100%; display: flex; justify-content: space-between; font-size: .85rem; color: var(--muted); }
.speakers, .log { list-style: none; padding: 0; margin: 0; }
.speakers { display: flex; flex-direction: column; justify-content: center; padding: 10px 14px; background: var(--soft); border: 1px solid var(--line); border-radius: 9px; }
.speakers.receiving { background: var(--ok-bg); color: var(--ok); }
.speakers li.self { color: var(--talk); }
.speakers li { display: flex; align-items: center; gap: 8px; flex-wrap: wrap; font-size: .9rem; }
.idle-line { justify-content: center; }
.ptt { width: 100%; min-height: 158px; display: flex; align-items: center; justify-content: center; gap: 18px; padding: 20px; border-radius: 12px; background: var(--accent); border: 3px solid transparent; color: white; user-select: none; -webkit-user-select: none; -webkit-touch-callout: none; touch-action: none; transition: background .1s, transform .1s; }
.ptt > .ui-icon { width: 42px; height: 54px; }
.ptt-copy { display: grid; gap: 4px; text-align: left; }
.ptt-copy strong { font-size: clamp(1.3rem, 2vw, 2rem); }
.ptt-copy small { font-size: .8rem; font-weight: 400; }
.ptt kbd { padding: 4px 8px; border: 1px solid #ffffffb0; border-radius: 6px; font: 600 .85rem/1.4 inherit; }
.ptt:not(:disabled):active, .ptt.requesting { transform: scale(.985); }
.ptt.on, .ptt.on:hover { background: #c52a31; border-color: #ffb5b8; }
.ptt.full { background: #70501b; }
.ptt.preempt { border-color: #e6b354; }
.volume { display: flex; align-items: center; gap: 10px; margin: auto 0 0; }
.volume input { flex: 1; min-width: 50px; }
.volume output { min-width: 42px; text-align: right; }
.transmit-note { display: flex; align-items: center; gap: 12px; padding: 14px 20px; margin: 18px 0 0; font-size: .9rem; }
.tts-panel h2 { margin-bottom: 2px; }
.tts-panel > .muted { margin-bottom: 20px; }
.tts-send { width: 100%; }
.tts-destination { display: flex; align-items: center; gap: 10px; padding: 8px; border: 1px solid var(--line); border-radius: 8px; }
.tts-destination .channel-badge { width: 40px; height: 40px; }
.char-count { text-align: right; margin: 3px 0 12px; }
.voice-info { margin: 20px 0; padding: 10px; border: 1px solid var(--line); border-radius: 7px; }
.voice-info summary { cursor: pointer; font-size: .9rem; }
.voice-info p { margin: 12px 0 0; }
.log-title { margin: 20px 0 10px; padding-top: 18px; border-top: 1px solid var(--line); }
.log { max-height: 280px; overflow-y: auto; }
.log li { border-bottom: 1px solid var(--line); padding: 10px 0; overflow-wrap: anywhere; }
.log p { font-size: .9rem; margin: 8px 0 0; }
.log-channel { padding: 2px 7px; border: 1px solid var(--line); border-radius: 4px; }
.tag { font-size: .7rem; padding: 1px 6px; border-radius: 4px; color: var(--warn); background: var(--warn-bg); }
.tag.su { background: #eee4ff; color: #603b91; }
@media (max-width: 1150px) { .radio-layout.with-tts { grid-template-columns: 1fr; } .tts-panel { max-width: none; } }
@media (max-width: 760px) {
  /* スマホでは PTT ボタンが最初の画面に収まるよう、上の部分を詰める */
  .sides.two { grid-template-columns: 1fr; }
  .radio-toolbar { padding: 10px 12px; gap: 8px; margin-bottom: 12px; }
  .connection-controls { margin-left: 0; gap: 10px; }
  .connection-controls button { min-height: 38px; padding: 6px 14px; }
  .mode-controls { flex-basis: 100%; gap: 10px; }
  .mode-hint { display: none; }
  .side { gap: 12px; }
  .side-heading h2 { font-size: 1.1rem; }
  .side-letter { width: 32px; height: 32px; font-size: 1.2rem; }
  .channel-select { gap: 10px; }
  .channel-select select { min-height: 50px; font-size: 1.1rem; }
  .channel-select .channel-badge { width: 50px; height: 50px; }
  .channel-counts { margin-top: -4px; }
  .ptt { min-height: 130px; }
  .transmit-note { margin-top: 12px; padding: 10px 14px; }
}
/* タッチ端末ではキーボードの案内を出さない */
@media (max-width: 760px), (hover: none) and (pointer: coarse) { .ptt kbd { display: none; } }
</style>
