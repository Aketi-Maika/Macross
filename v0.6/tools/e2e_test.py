"""Macross 実機 E2E テスト（aiortc で 2 人分のブラウザを模擬し、実際の音声で確認する）

確認内容: HTTPS ログイン / WSS / Janus への参加 / PTT と発言枠 / MCU ミックス / LR の分離 /
          読み上げ（Open JTalk → play_file）と連投制限 / チャンネル削除時の切り離し

使い方（サーバと UDP 20000-20200 で通信できる PC から）:
  pip install aiortc numpy websockets
  E2E_HOST=10.40.0.13 E2E_ADMIN_PW=<admin のパスワード> python tools/e2e_test.py
環境変数:
  E2E_HOST       サーバのドメインまたは IP
  E2E_ADMIN_PW   ID 0001 の現在のパスワード
  E2E_ADMIN_NEW  0001 が初期パスワードのままの場合の変更後パスワード
注意: テスト用ユーザ「E2Eテスト」を発行し、終了時に無効化する。ch1 の発言枠を 1 に設定する。
"""
import asyncio
import fractions
import json
import os
import ssl
import sys
import time
import urllib.request
import http.cookiejar

import av
import numpy as np
import websockets
from aiortc import MediaStreamTrack, RTCPeerConnection, RTCSessionDescription

HOST = os.environ.get("E2E_HOST", "127.0.0.1")
BASE = f"https://{HOST}"
CTX = ssl.create_default_context()
CTX.check_hostname = False
CTX.verify_mode = ssl.CERT_NONE
results = []


def check(label, cond, detail=""):
    results.append(cond)
    print(("OK  " if cond else "NG  ") + label, "" if cond else detail, flush=True)


class Http:
    def __init__(self):
        self.jar = http.cookiejar.CookieJar()
        self.op = urllib.request.build_opener(urllib.request.HTTPCookieProcessor(self.jar),
                                              urllib.request.HTTPSHandler(context=CTX))

    def call(self, method, path, body=None):
        data = json.dumps(body).encode() if body is not None else None
        req = urllib.request.Request(BASE + path, data=data, method=method,
                                     headers={"Content-Type": "application/json"} if data else {})
        try:
            with self.op.open(req, timeout=15) as r:
                raw = r.read()
                return r.status, (json.loads(raw) if raw else None)
        except urllib.error.HTTPError as e:
            return e.code, json.loads(e.read() or b"{}")

    def cookie(self):
        return "; ".join(f"{c.name}={c.value}" for c in self.jar)


class Tone(MediaStreamTrack):
    """440Hz（silent=True なら無音）"""
    kind = "audio"

    def __init__(self, silent=False):
        super().__init__()
        self.silent, self.n, self.t0 = silent, 0, None

    async def recv(self):
        if self.t0 is None:
            self.t0 = time.time()
        wait = self.t0 + self.n * 0.02 - time.time()
        if wait > 0:
            await asyncio.sleep(wait)
        t = (np.arange(960) + self.n * 960) / 48000
        s = np.zeros(960) if self.silent else 0.3 * np.sin(2 * np.pi * 440 * t)
        frame = av.AudioFrame.from_ndarray((s * 32767).astype(np.int16).reshape(1, -1), format="s16", layout="mono")
        frame.sample_rate, frame.pts, frame.time_base = 48000, self.n * 960, fractions.Fraction(1, 48000)
        self.n += 1
        return frame


class Leg:
    def __init__(self):
        self.pc = RTCPeerConnection()
        self.levels = []  # (time, rms)

    async def consume(self, track):
        while True:
            try:
                f = await track.recv()
            except Exception:
                return
            a = f.to_ndarray().astype(np.float32)
            self.levels.append((time.time(), float(np.sqrt(np.mean(a * a)))))

    def rms(self, t0, t1):
        v = [r for t, r in self.levels if t0 <= t <= t1]
        return max(v) if v else -1.0


class User:
    def __init__(self, name, http):
        self.name, self.http, self.msgs, self.legs = name, http, [], {}

    async def open(self):
        self.ws = await websockets.connect(f"wss://{HOST}/ws", ssl=CTX,
                                           additional_headers={"Cookie": self.http.cookie()})
        self.reader = asyncio.create_task(self._read())

    async def _read(self):
        async for raw in self.ws:
            self.msgs.append(json.loads(raw))

    async def send(self, **m):
        await self.ws.send(json.dumps(m))

    async def wait(self, pred, timeout=15):
        start = len(self.msgs)
        end = time.time() + timeout
        seen = 0
        while time.time() < end:
            for m in self.msgs[seen:]:
                if pred(m):
                    return m
            seen = len(self.msgs)
            await asyncio.sleep(0.05)
        return None

    async def join(self, side, channel, silent=True):
        leg = Leg()
        self.legs[side] = leg
        leg.pc.addTrack(Tone(silent))
        leg.pc.on("track", lambda tr: asyncio.ensure_future(leg.consume(tr)))
        await leg.pc.setLocalDescription(await leg.pc.createOffer())
        mark = len(self.msgs)
        await self.send(type="join", side=side, channel=channel, sdp=leg.pc.localDescription.sdp)
        m = await self.wait(lambda m: m.get("side") == side and m["type"] in ("answer", "left"))
        if m and m["type"] == "answer":
            await leg.pc.setRemoteDescription(RTCSessionDescription(m["sdp"], "answer"))
        return m

    async def close(self):
        for leg in self.legs.values():
            await leg.pc.close()
        await self.ws.close()


async def main():
    admin_pw, admin_new = os.environ["E2E_ADMIN_PW"], os.environ.get("E2E_ADMIN_NEW")
    ha = Http()
    s, u = ha.call("POST", "/api/login", {"id": "0001", "password": admin_pw})
    check("HTTPS（Caddy 自己署名）経由で admin ログイン", s == 200, (s, u))
    if u.get("must_change"):
        s, _ = ha.call("POST", "/api/password", {"current": admin_pw, "new": admin_new})
        check("admin の初期パスワードを変更", s == 200)

    s, c = ha.call("GET", "/api/admin/channels")
    names = {x["name"]: x["id"] for x in c["channels"]}
    ch1, ch2 = names["ch1"], names["ch2"]
    ha.call("PATCH", f"/api/admin/channels/{ch1}", {"max_speakers": 1})

    s, r = ha.call("POST", "/api/admin/users", {"name": "E2Eテスト", "furigana": "いーつーいー"})
    hb = Http()
    hb.call("POST", "/api/login", {"id": r["id"], "password": r["initial_password"]})
    hb.call("POST", "/api/password", {"current": r["initial_password"], "new": "e2e-test-pass-1"})
    b_id = r["id"]

    a, b = User("A", ha), User("B", hb)
    await a.open()
    await b.open()
    check("WSS 接続で state 受信", await a.wait(lambda m: m["type"] == "state") is not None)

    m = await a.join("L", ch1, silent=False)
    check("A: L=ch1 に参加（Janus から answer）", m and m["type"] == "answer", m)
    m = await b.join("L", ch1)
    check("B: L=ch1 に参加", m and m["type"] == "answer", m)
    m = await b.join("R", ch2)
    check("B: R=ch2 に参加（LR受聴）", m and m["type"] == "answer", m)
    await asyncio.sleep(4)  # ICE / DTLS 確立待ち

    t0 = time.time()
    await asyncio.sleep(2)
    base_l = b.legs["L"].rms(t0, time.time())
    check("B: 受信音声が届いている（RTP 受信）", len(b.legs["L"].levels) > 50, len(b.legs["L"].levels))

    await a.send(type="ptt", side="L", on=True)
    m = await a.wait(lambda m: m["type"] == "ptt")
    check("A: PTT 許可", m and m["granted"], m)
    st = await b.wait(lambda m: m["type"] == "state" and any(s["id"] == "0001" for c in m["channels"] if c["id"] == ch1 for s in c["speakers"]))
    check("B: ch1 の発言中に A が表示される", st is not None)

    await b.send(type="ptt", side="L", on=True)
    m = await b.wait(lambda m: m["type"] == "ptt")
    check("B: 発言枠 1 が埋まっているので拒否", m and not m["granted"] and "発言枠" in (m.get("reason") or ""), m)

    t1 = time.time()
    await asyncio.sleep(3)
    t2 = time.time()
    await a.send(type="ptt", side="L", on=False)
    talk_l = b.legs["L"].rms(t1 + 0.5, t2)
    talk_r = b.legs["R"].rms(t1 + 0.5, t2)
    print(f"    B の L 受信レベル: 無音時 {base_l:.0f} → A 発言中 {talk_l:.0f} / R(ch2) {talk_r:.0f}")
    check("B: A の声（440Hz）が L に届く（MCU ミックス）", talk_l > max(300, base_l * 5), (base_l, talk_l))
    check("B: ch1 の声は R(ch2) には届かない", talk_r < talk_l / 5, (talk_r, talk_l))

    await asyncio.sleep(1)
    mark = len(b.msgs)
    await b.send(type="tts", side="R", text="これは読み上げのテストです")
    await b.send(type="tts", side="R", text="連投")
    m = await b.wait(lambda m: m["type"] == "error" and "待って" in m["message"], timeout=3)
    check("読み上げの連投は間隔制限で拒否", m is not None)
    m = await b.wait(lambda m: m["type"] == "tts", timeout=20)
    check("B: R(ch2) へ読み上げ（Open JTalk → play_file）", m is not None, b.msgs[-3:])
    t3 = time.time()
    await asyncio.sleep(3)
    tts_r = b.legs["R"].rms(t3, time.time())
    tts_l = b.legs["L"].rms(t3, time.time())
    print(f"    読み上げ中の B 受信レベル: R(ch2) {tts_r:.0f} / L(ch1) {tts_l:.0f}")
    check("B: 読み上げ音声が R に届く", tts_r > 300, tts_r)
    check("A: ch2 を聴いていないので読み上げ通知は来ない", not any(x["type"] == "tts" for x in a.msgs))

    s, nc = ha.call("POST", "/api/admin/channels", {"name": "e2e削除テスト"})
    m = await a.join("R", nc["id"])
    check("A: R=新チャンネルに参加", m and m["type"] == "answer", m)
    await asyncio.sleep(2)
    ha.call("DELETE", f"/api/admin/channels/{nc['id']}")
    m = await a.wait(lambda m: m["type"] == "left" and m["side"] == "R")
    check("チャンネル削除で A の R が切り離される", m and "削除" in m["reason"], m)

    await a.close()
    await b.close()
    ha.call("PATCH", f"/api/admin/users/{b_id}", {"disabled": True})
    await asyncio.sleep(4)  # 録音変換待ち
    print(f"\n結果: {sum(results)}/{len(results)} OK")


asyncio.run(main())
