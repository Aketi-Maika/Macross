#!/usr/bin/env python3
"""Macross.yaml から各コンテナの設定ファイルを生成する（docker compose の config サービス）。

使い方: render.py <Macross.yaml> <出力フォルダ>

生成するもの:
  <出力>/janus/*.jcfg   Janus Gateway
  <出力>/Caddyfile      Caddy（HTTPS / ドメイン）
  <出力>/grafana.ini    Grafana（URL）
"""
import os
import sys
from urllib.parse import urlparse

import yaml


def fail(msg):
    print(f"[Macross.yaml エラー] {msg}", file=sys.stderr)
    sys.exit(1)


def q(value):
    """libconfig の文字列リテラル"""
    return '"' + str(value).replace("\\", "\\\\").replace('"', '\\"') + '"'


def write(path, body):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path, "w", encoding="utf-8") as f:
        f.write(body.lstrip())
    os.chmod(path, 0o644)


# ---------------------------------------------------------------- Janus
def render_janus(cfg, out):
    janus = cfg.get("janus") or {}
    channel = cfg.get("channel") or {}
    audio = cfg.get("audio") or {}

    url = urlparse(janus.get("url", "http://127.0.0.1:8088/janus"))
    public_ip = str(janus.get("public_ip") or "").strip()
    rtp_range = str(janus.get("rtp_port_range") or "20000-20200")
    if not public_ip:
        print("[警告] janus.public_ip が未設定です。外部からの音声が届かない可能性があります", file=sys.stderr)

    nat = []
    if public_ip:
        nat += [f"nat_1_1_mapping = {q(public_ip)}", "keep_private_host = true"]
    nat.append('ice_ignore_list = "vmnet,docker,veth,br-"')
    nat_block = "\n    ".join(nat)

    write(f"{out}/janus.jcfg", f"""
general: {{
    configs_folder = {q(out)}
    log_to_stdout = true
    debug_level = 4
    session_timeout = 60
    server_name = "Macross"
}}
media: {{
    rtp_port_range = {q(rtp_range)}
}}
nat: {{
    {nat_block}
}}
plugins: {{
    disable = "libjanus_echotest.so,libjanus_videocall.so,libjanus_videoroom.so,libjanus_streaming.so,libjanus_recordplay.so,libjanus_sip.so,libjanus_nosip.so,libjanus_textroom.so,libjanus_voicemail.so,libjanus_duktape.so,libjanus_lua.so"
}}
transports: {{
    disable = "libjanus_websockets.so,libjanus_pfunix.so,libjanus_rabbitmq.so,libjanus_mqtt.so,libjanus_nanomsg.so"
}}
events: {{
    broadcast = false
}}
""")
    write(f"{out}/janus.transport.http.jcfg", f"""
general: {{
    json = "compact"
    base_path = {q(url.path or "/janus")}
    http = true
    port = {int(url.port or 8088)}
    ip = {q(url.hostname or "127.0.0.1")}
    https = false
}}
admin: {{
    admin_http = false
}}
cors: {{
}}
""")
    write(f"{out}/janus.plugin.audiobridge.jcfg", f"""
general: {{
    events = false
}}
room-{int(channel.get("room_id", 1001))}: {{
    description = {q(channel.get("name", "ch1"))}
    sampling_rate = {int(audio.get("sampling_rate", 48000))}
    default_bitrate = {int(audio.get("bitrate", 32000))}
    record = false
}}
""")
    return f"public_ip={public_ip or '未設定'}, UDP {rtp_range}"


# ---------------------------------------------------------------- Caddy
ROUTES = """
	encode gzip

	# 録音ブラウザ（File Browser）
	handle /files* {
		import admin_only
		reverse_proxy 127.0.0.1:8090
	}

	# 監視ダッシュボード（Grafana）
	handle /grafana* {
		import admin_only
		reverse_proxy 127.0.0.1:3000
	}

	# Macross
	handle {
		reverse_proxy %(app)s
	}
"""


def render_caddy(cfg, server, domain, mode, path):
    listen = str(server.get("listen") or "127.0.0.1:8080")
    app = listen.replace("0.0.0.0", "127.0.0.1")
    routes = ROUTES % {"app": app}
    head = f"""
# このファイルは Macross.yaml から自動生成されます。直接編集しないでください。

# admin でログインしているかを Macross に問い合わせる
(admin_only) {{
	forward_auth {app} {{
		uri /api/auth/admin
	}}
}}
"""
    if mode == "caddy":
        email = str(server.get("acme_email") or "").strip()
        glob = f"{{\n\temail {email}\n}}\n" if email else ""
        body = f"{glob}{head}\n{domain} {{{routes}}}\n"
    else:
        # Cloudflare Tunnel が TLS を終端する。Caddy は 127.0.0.1:8081 で HTTP のみ
        body = f"{{\n\tauto_https off\n}}\n{head}\nhttp://:8081 {{\n\tbind 127.0.0.1\n{routes}}}\n"
    write(path, body)


# ---------------------------------------------------------------- Grafana
def render_grafana(domain, path):
    write(path, f"""
; このファイルは Macross.yaml から自動生成されます。直接編集しないでください。
[server]
http_addr = 127.0.0.1
http_port = 3000
domain = {domain}
root_url = https://{domain}/grafana/
serve_from_sub_path = true

; 認証は Caddy が Macross の admin ログインで行う
[auth]
disable_login_form = true

[auth.anonymous]
enabled = true
org_role = Viewer

[dashboards]
default_home_dashboard_path = /var/lib/grafana/dashboards/macross.json

[analytics]
reporting_enabled = false
check_for_updates = false
""")


def main():
    src, out = sys.argv[1], sys.argv[2]
    try:
        with open(src, encoding="utf-8") as f:
            cfg = yaml.safe_load(f) or {}
    except FileNotFoundError:
        fail(f"{src} が見つかりません")
    except yaml.YAMLError as e:
        fail(f"形式が不正です: {e}")

    server = cfg.get("server") or {}
    domain = str(server.get("domain") or "").strip().lower()
    mode = str(server.get("https") or "caddy").strip().lower()
    if not domain:
        fail("server.domain にドメインを設定してください（例: macross.build-maika.jp）")
    if mode not in ("caddy", "tunnel"):
        fail(f"server.https は caddy か tunnel を指定してください（現在: {mode}）")

    janus_info = render_janus(cfg, f"{out}/janus")
    render_caddy(cfg, server, domain, mode, f"{out}/Caddyfile")
    render_grafana(domain, f"{out}/grafana.ini")

    entry = f"https://{domain}" + ("" if mode == "caddy" else "（Cloudflare Tunnel → http://127.0.0.1:8081）")
    print(f"設定を生成しました: {entry} / Janus {janus_info}")


if __name__ == "__main__":
    main()
