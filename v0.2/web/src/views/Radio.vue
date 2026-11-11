<script setup>
import { ref, reactive, computed, onBeforeUnmount, onMounted } from 'vue'
import { api, session } from '../api.js'

const SIDES = ['L', 'R']
const status = ref('idle') // idle | connecting | connected
const message = ref('')
const online = ref(0)
const maxConnections = ref(0)
const channels = ref([])
const lr = ref(loadPref('lr', false))
const talking = reactive({ side: null, state: 'off' }) // state: off | requesting | on
const cfg = reactive({ ice_servers: [], tts: false, tts_max_chars: 200 })
const voice = ref(null)
const ttsText = ref('')
const ttsSide = ref('L')
const ttsLog = ref([])
const panningOk = ref(true)

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

function slotsFull(side) {
  const ch = channelById(sides[side].channel)
  if (!ch || talking.side === side) return false
  return ch.speakers.length >= ch.max_speakers
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
  if (['INPUT', 'TEXTAREA', 'SELECT'].includes(e.target.tagName)) return
  const side = e.code === 'Space' ? 'L' : e.code === 'Enter' && lr.value ? 'R' : null
  if (!side) return
  e.preventDefault()
  if (e.type === 'keydown' && !e.repeat) pttDown(side)
  if (e.type === 'keyup' && pressed === side) pttUp()
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
  <div class="container">
    <div class="panel">
      <div class="row">
        <h1 class="grow">無線</h1>
        <span class="badge" :class="status">{{
          { idle: '未接続', connecting: '接続中…', connected: '接続済み' }[status]
        }}</span>
      </div>
      <p v-if="status === 'connected'" class="muted">接続 {{ online }} / {{ maxConnections }} 人</p>
      <div class="row" style="margin-top: 8px">
        <button v-if="status !== 'connected'" :disabled="status === 'connecting'" @click="connect">接続する</button>
        <button v-else class="secondary" @click="disconnect()">切断する</button>
        <label class="check">
          <input type="checkbox" :checked="lr" @change="toggleLr" />
          LR受聴（2チャンネルを左右で聴く）
        </label>
      </div>
      <p v-if="lr" class="muted">イヤホン推奨。左耳と右耳で別のチャンネルが聞こえます。</p>
      <p v-if="lr && !panningOk" class="error">このブラウザは左右の振り分けに対応していないため、両方とも中央で聞こえます。</p>
      <p v-if="message" class="error">{{ message }}</p>
    </div>

    <div class="sides" :class="{ two: lr }">
      <div v-for="side in activeSides" :key="side" class="panel side">
        <div class="row">
          <h2 class="grow">{{ sideLabel(side) }}</h2>
          <span class="badge" :class="{ connected: sides[side].state === 'on', connecting: sides[side].state === 'joining' }">{{
            { off: '未参加', joining: '参加中…', on: '参加中' }[sides[side].state]
          }}</span>
        </div>
        <select
          :value="sides[side].channel ?? ''"
          :disabled="status !== 'connected'"
          @change="onSelect(side, $event.target.value)"
        >
          <option value="">（聴かない）</option>
          <option
            v-for="c in channels"
            :key="c.id"
            :value="c.id"
            :disabled="lr && c.id === sides[side === 'L' ? 'R' : 'L'].channel"
          >
            {{ c.name }}（{{ c.listeners }}人）
          </option>
        </select>

        <template v-if="channelById(sides[side].channel)">
          <p class="muted">
            発言枠 {{ channelById(sides[side].channel).speakers.length }} / {{ channelById(sides[side].channel).max_speakers }}
          </p>
          <ul class="speakers">
            <li v-for="s in channelById(sides[side].channel).speakers" :key="s.id">
              <span class="dot"></span>{{ s.name }} <span class="muted">({{ s.id }})</span>
              <span v-if="s.id === session.user?.id" class="muted">← あなた</span>
            </li>
          </ul>
        </template>

        <div class="ptt-area">
          <button
            class="ptt"
            :class="{
              on: talking.side === side && talking.state === 'on',
              requesting: talking.side === side && talking.state === 'requesting',
              full: slotsFull(side),
              small: lr,
            }"
            :disabled="sides[side].state !== 'on' || (talking.side && talking.side !== side)"
            @pointerdown="pttDown(side, $event)"
            @pointerup="pttUp"
            @pointercancel="pttUp"
            @lostpointercapture="pttUp"
            @contextmenu.prevent
          >
            <span v-if="talking.side === side && talking.state === 'on'">送信中</span>
            <span v-else-if="talking.side === side">…</span>
            <span v-else-if="slotsFull(side)">発言枠いっぱい</span>
            <span v-else>押して話す</span>
          </button>
        </div>
        <label class="volume">
          音量
          <input type="range" min="0" max="1" step="0.05" :value="sides[side].volume" @input="setVolume(side, $event.target.value)" />
        </label>
      </div>
    </div>
    <p class="muted center">
      ボタンを押している間だけ送信・録音されます（PC は スペースキー<span v-if="lr">＝左、Enter キー＝右</span>）
    </p>

    <div v-if="status === 'connected'" class="panel">
      <h2>読み上げ（TTS）</h2>
      <p v-if="!cfg.tts" class="muted">サーバで読み上げが無効になっています。</p>
      <template v-else>
        <form class="row" @submit.prevent="sendTts">
          <select v-if="lr" v-model="ttsSide" style="width: auto">
            <option v-for="s in SIDES" :key="s" :value="s">{{ channelById(sides[s].channel)?.name ?? s }} へ</option>
          </select>
          <input v-model="ttsText" class="grow" :maxlength="cfg.tts_max_chars" placeholder="入力した文章をあなたの声で読み上げます" />
          <button type="submit" :disabled="!ttsText.trim()">送信</button>
        </form>
        <p v-if="voice" class="muted">
          あなたの声: {{ voice.model }} / 高さ {{ voice.pitch > 0 ? '+' : '' }}{{ voice.pitch }} / 速さ ×{{ voice.speed }} / 声質
          {{ voice.alpha }}
        </p>
        <ul class="log">
          <li v-for="t in ttsLog" :key="t.key">
            <span class="muted">{{ t.time }} [{{ t.channelName }}]</span> {{ t.name }}: {{ t.text }}
          </li>
        </ul>
      </template>
    </div>

    <audio ref="audioElL" autoplay playsinline></audio>
    <audio ref="audioElR" autoplay playsinline></audio>
  </div>
</template>

<style scoped>
.badge {
  font-size: .8rem;
  padding: 4px 10px;
  border-radius: 999px;
  background: var(--line);
  white-space: nowrap;
}
.badge.connected { background: var(--ok); color: #fff; }
.badge.connecting { background: var(--warn); color: #000; }
.check { display: flex; align-items: center; gap: 6px; margin: 0; }
.check input { width: auto; }
.center { text-align: center; }

.sides { display: grid; gap: 12px; }
.sides.two { grid-template-columns: 1fr 1fr; }
@media (max-width: 560px) { .sides.two { grid-template-columns: 1fr; } }
.side h2 { margin: 0; }

.ptt-area { text-align: center; margin: 16px 0 8px; }
.ptt {
  width: min(70vw, 240px);
  height: min(70vw, 240px);
  border-radius: 50%;
  font-size: 1.3rem;
  font-weight: 700;
  background: #2a3240;
  border: 4px solid var(--line);
  user-select: none;
  -webkit-user-select: none;
  -webkit-touch-callout: none;
  touch-action: none;
  transition: background .1s, transform .1s;
}
.ptt.small { width: min(38vw, 180px); height: min(38vw, 180px); font-size: 1.1rem; }
@media (max-width: 560px) { .ptt.small { width: min(60vw, 200px); height: min(60vw, 200px); } }
.ptt:not(:disabled):active, .ptt.requesting { transform: scale(.97); }
.ptt.on { background: var(--talk); border-color: #ff8a8d; }
.ptt.full { background: #3a3020; }

.volume { display: flex; align-items: center; gap: 8px; margin: 0; }
.volume input { flex: 1; }

.speakers, .log { list-style: none; padding: 0; margin: 0; }
.speakers li { padding: 2px 0; }
.log { margin-top: 8px; max-height: 240px; overflow-y: auto; }
.log li { padding: 4px 0; border-bottom: 1px solid var(--line); word-break: break-all; }
.dot {
  display: inline-block;
  width: 8px;
  height: 8px;
  margin-right: 8px;
  border-radius: 50%;
  background: var(--talk);
  animation: blink 1s infinite;
}
@keyframes blink { 50% { opacity: .3; } }
</style>
