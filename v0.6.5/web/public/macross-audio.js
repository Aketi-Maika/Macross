// Macross v0.6.5: 音声を WebSocket で運ぶとき（audio.transport: websocket）の AudioWorklet
//
//   macross-capture: マイク → 16kHz に間引き → G.711 μ-law 20ms（320 バイト）ごとに main スレッドへ
//   macross-player : サーバから届いた μ-law 16kHz → ゆらぎ吸収のバッファ → AudioContext のレートで出力
//
// サーバとの間は 16kHz・20ms・μ-law（server/src/media.rs と同じ形）。

const RATE = 16000
const FRAME = 320

function ulawEncode(sample) {
  const BIAS = 0x84
  const CLIP = 32635
  let x = Math.max(-32768, Math.min(32767, Math.round(sample * 32767)))
  let sign = 0
  if (x < 0) {
    x = -x
    sign = 0x80
  }
  x = Math.min(x, CLIP) + BIAS
  const high = 31 - Math.clz32(x)
  const exp = Math.max(0, Math.min(7, high - 7))
  const mantissa = (x >> (exp + 3)) & 0x0f
  return ~(sign | (exp << 4) | mantissa) & 0xff
}

const ULAW_TABLE = new Float32Array(256)
for (let i = 0; i < 256; i++) {
  const u = ~i & 0xff
  const exp = (u >> 4) & 0x07
  const mantissa = u & 0x0f
  const x = (((mantissa << 3) + 0x84) << exp) - 0x84
  ULAW_TABLE[i] = (u & 0x80 ? -x : x) / 32768
}

class Capture extends AudioWorkletProcessor {
  constructor() {
    super()
    this.on = false
    this.step = sampleRate / RATE // 入力何サンプルで 16kHz の 1 サンプルか
    this.acc = 0
    this.sum = 0
    this.count = 0
    this.frame = new Uint8Array(FRAME)
    this.pos = 0
    this.port.onmessage = (e) => {
      this.on = !!e.data.on
      this.pos = 0
      this.acc = this.sum = this.count = 0
    }
  }

  process(inputs) {
    const ch = inputs[0] && inputs[0][0]
    if (!this.on || !ch) return true
    for (let i = 0; i < ch.length; i++) {
      // 区間の平均で間引く（簡易なローパスを兼ねる）
      this.sum += ch[i]
      this.count++
      this.acc += 1
      if (this.acc >= this.step) {
        this.acc -= this.step
        this.frame[this.pos++] = ulawEncode(this.sum / this.count)
        this.sum = this.count = 0
        if (this.pos === FRAME) {
          const out = this.frame
          this.port.postMessage(out, [out.buffer])
          this.frame = new Uint8Array(FRAME)
          this.pos = 0
        }
      }
    }
    return true
  }
}

class Player extends AudioWorkletProcessor {
  constructor() {
    super()
    this.step = RATE / sampleRate // 出力 1 サンプルで 16kHz 側をどれだけ進めるか
    this.buf = new Float32Array(RATE * 2) // 2 秒ぶんのリングバッファ
    this.read = 0 // 読み出し位置（小数）
    this.write = 0 // 書き込み位置（整数・累計）
    this.playing = false
    this.START = RATE * 0.08 // 80ms 溜まったら鳴らし始める（ネットのゆらぎを吸収）
    this.MAX = RATE * 0.4 // 400ms 以上遅れたら追いつく
    this.port.onmessage = (e) => {
      const f = new Uint8Array(e.data)
      const n = this.buf.length
      for (let i = 0; i < f.length; i++) this.buf[(this.write + i) % n] = ULAW_TABLE[f[i]]
      this.write += f.length
      if (this.write - this.read > this.MAX) this.read = this.write - this.START
    }
  }

  process(inputs, outputs) {
    const out = outputs[0][0]
    const n = this.buf.length
    let avail = this.write - this.read
    if (!this.playing && avail >= this.START) this.playing = true
    for (let i = 0; i < out.length; i++) {
      if (!this.playing || avail < 2) {
        out[i] = 0
        this.playing = false
        continue
      }
      const p = Math.floor(this.read)
      const t = this.read - p
      const a = this.buf[p % n]
      const b = this.buf[(p + 1) % n]
      out[i] = a + (b - a) * t
      this.read += this.step
      avail = this.write - this.read
    }
    for (let c = 1; c < outputs[0].length; c++) outputs[0][c].set(out)
    return true
  }
}

registerProcessor('macross-capture', Capture)
registerProcessor('macross-player', Player)
