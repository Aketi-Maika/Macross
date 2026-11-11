<script setup>
import { ref, computed, onBeforeUnmount, onMounted } from 'vue'
import { api, session } from '../api.js'

const status = ref('idle') // idle | connecting | connected
const talking = ref('off') // off | requesting | on
const message = ref('')
const channel = ref('')
const online = ref(0)
const maxConnections = ref(0)
const maxSpeakers = ref(1)
const speakers = ref([])
const audioEl = ref(null)

let pc = null
let ws = null
let micStream = null
let micTrack = null
let audioCtx = null
let wakeLock = null
let pressed = false

const othersTalking = computed(() => speakers.value.filter((s) => s.id !== session.user?.id))
const slotsFull = computed(() => talking.value === 'off' && speakers.value.length >= maxSpeakers.value)

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

async function connect() {
  message.value = ''
  status.value = 'connecting'
  try {
    // iOS Safari の自動再生制限のため、クリック直後に再生を開始しておく
    audioCtx = new (window.AudioContext || window.webkitAudioContext)()
    const remote = new MediaStream()
    audioEl.value.srcObject = remote
    audioEl.value.play().catch(() => {})

    micStream = await navigator.mediaDevices.getUserMedia({
      audio: { echoCancellation: true, noiseSuppression: true, autoGainControl: true },
      video: false,
    })
    micTrack = micStream.getAudioTracks()[0]
    micTrack.enabled = false

    const cfg = await api.get('/api/config')
    channel.value = cfg.channel
    pc = new RTCPeerConnection({ iceServers: cfg.ice_servers })
    pc.addTrack(micTrack, micStream)
    pc.ontrack = (e) => {
      remote.addTrack(e.track)
      audioEl.value.play().catch(() => {})
    }
    pc.onconnectionstatechange = () => {
      if (pc && ['failed', 'closed'].includes(pc.connectionState)) {
        disconnect('音声の接続が切れました')
      }
    }
    await pc.setLocalDescription(await pc.createOffer())
    await waitIceGathering(pc)

    const proto = location.protocol === 'https:' ? 'wss' : 'ws'
    ws = new WebSocket(`${proto}://${location.host}/ws`)
    ws.onopen = () => ws.send(JSON.stringify({ type: 'join', sdp: pc.localDescription.sdp }))
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

async function onMessage(msg) {
  switch (msg.type) {
    case 'answer':
      await pc.setRemoteDescription({ type: 'answer', sdp: msg.sdp })
      status.value = 'connected'
      break
    case 'state':
      channel.value = msg.channel
      online.value = msg.online
      maxConnections.value = msg.max_connections
      maxSpeakers.value = msg.max_speakers
      speakers.value = msg.speakers
      break
    case 'ptt':
      if (msg.granted) {
        if (pressed) {
          micTrack.enabled = true
          talking.value = 'on'
          beep(880)
        } else {
          send({ type: 'ptt', on: false })
        }
      } else {
        talking.value = 'off'
        message.value = msg.reason || '発言できません'
        beep(220, 150)
      }
      break
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

function pttDown(e) {
  if (status.value !== 'connected' || pressed) return
  e?.preventDefault?.()
  if (e?.pointerId !== undefined) e.currentTarget.setPointerCapture(e.pointerId)
  pressed = true
  message.value = ''
  talking.value = 'requesting'
  send({ type: 'ptt', on: true })
}

function pttUp() {
  if (!pressed) return
  pressed = false
  if (micTrack) micTrack.enabled = false
  if (talking.value !== 'off') {
    if (talking.value === 'on') beep(440)
    send({ type: 'ptt', on: false })
  }
  talking.value = 'off'
}

function onKey(e) {
  if (e.code !== 'Space' || e.target.tagName === 'INPUT') return
  e.preventDefault()
  if (e.type === 'keydown' && !e.repeat) pttDown()
  if (e.type === 'keyup') pttUp()
}

function disconnect(reason = '') {
  pressed = false
  talking.value = 'off'
  status.value = 'idle'
  speakers.value = []
  if (reason) message.value = reason
  if (ws) {
    ws.onclose = null
    ws.close()
    ws = null
  }
  if (pc) {
    pc.close()
    pc = null
  }
  micStream?.getTracks().forEach((t) => t.stop())
  micStream = micTrack = null
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
        <h1 class="grow">チャンネル {{ channel || '—' }}</h1>
        <span class="badge" :class="status">{{
          { idle: '未接続', connecting: '接続中…', connected: '接続済み' }[status]
        }}</span>
      </div>
      <p v-if="status === 'connected'" class="muted">
        接続 {{ online }} / {{ maxConnections }} 人 ・ 発言枠 {{ speakers.length }} / {{ maxSpeakers }}
      </p>
      <div v-if="status !== 'connected'" style="margin-top: 8px">
        <button :disabled="status === 'connecting'" @click="connect">接続する</button>
      </div>
      <div v-else>
        <button class="secondary" @click="disconnect()">切断する</button>
      </div>
      <p v-if="message" class="error">{{ message }}</p>
    </div>

    <div class="ptt-area">
      <button
        class="ptt"
        :class="{ on: talking === 'on', requesting: talking === 'requesting', full: slotsFull }"
        :disabled="status !== 'connected'"
        @pointerdown="pttDown"
        @pointerup="pttUp"
        @pointercancel="pttUp"
        @lostpointercapture="pttUp"
        @contextmenu.prevent
      >
        <span v-if="talking === 'on'">送信中</span>
        <span v-else-if="talking === 'requesting'">…</span>
        <span v-else-if="slotsFull">発言枠いっぱい</span>
        <span v-else>押して話す</span>
      </button>
      <p class="muted">ボタンを押している間だけ送信・録音されます（PCはスペースキーでも可）</p>
    </div>

    <div v-if="status === 'connected'" class="panel">
      <h2>発言中</h2>
      <p v-if="speakers.length === 0" class="muted">なし</p>
      <ul v-else class="speakers">
        <li v-for="s in speakers" :key="s.id">
          <span class="dot"></span>{{ s.name }} <span class="muted">({{ s.id }})</span>
        </li>
      </ul>
      <p v-if="othersTalking.length" class="muted">受信中</p>
    </div>

    <audio ref="audioEl" autoplay playsinline></audio>
  </div>
</template>

<style scoped>
.badge {
  font-size: .8rem;
  padding: 4px 10px;
  border-radius: 999px;
  background: var(--line);
}
.badge.connected { background: var(--ok); color: #fff; }
.badge.connecting { background: var(--warn); color: #000; }

.ptt-area { text-align: center; margin: 24px 0; }
.ptt {
  width: min(70vw, 260px);
  height: min(70vw, 260px);
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
.ptt:not(:disabled):active, .ptt.requesting { transform: scale(.97); }
.ptt.on { background: var(--talk); border-color: #ff8a8d; }
.ptt.full { background: #3a3020; }

.speakers { list-style: none; padding: 0; margin: 0; }
.speakers li { padding: 4px 0; }
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
