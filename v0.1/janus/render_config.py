#!/usr/bin/env python3
"""Macross.yaml から Janus の設定ファイル (*.jcfg) を生成する。

使い方: render_config.py <Macross.yaml> <出力フォルダ>
"""
import os
import sys
from urllib.parse import urlparse

import yaml


def q(value):
    """libconfig の文字列リテラル"""
    return '"' + str(value).replace("\\", "\\\\").replace('"', '\\"') + '"'


def main():
    src, out = sys.argv[1], sys.argv[2]
    with open(src, encoding="utf-8") as f:
        cfg = yaml.safe_load(f) or {}

    janus = cfg.get("janus") or {}
    channel = cfg.get("channel") or {}
    audio = cfg.get("audio") or {}

    url = urlparse(janus.get("url", "http://127.0.0.1:8088/janus"))
    http_ip = url.hostname or "127.0.0.1"
    http_port = url.port or 8088
    base_path = url.path or "/janus"

    public_ip = str(janus.get("public_ip") or "").strip()
    rtp_range = str(janus.get("rtp_port_range") or "20000-20200")

    nat = []
    if public_ip:
        nat.append(f"nat_1_1_mapping = {q(public_ip)}")
        nat.append("keep_private_host = true")
    nat.append('ice_ignore_list = "vmnet,docker,veth,br-"')
    nat_block = "\n    ".join(nat)

    os.makedirs(out, exist_ok=True)
    files = {
        "janus.jcfg": f"""
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
""",
        "janus.transport.http.jcfg": f"""
general: {{
    json = "compact"
    base_path = {q(base_path)}
    http = true
    port = {int(http_port)}
    ip = {q(http_ip)}
    https = false
}}
admin: {{
    admin_http = false
}}
cors: {{
}}
""",
        "janus.plugin.audiobridge.jcfg": f"""
general: {{
    events = false
}}
room-{int(channel.get("room_id", 1001))}: {{
    description = {q(channel.get("name", "ch1"))}
    sampling_rate = {int(audio.get("sampling_rate", 48000))}
    default_bitrate = {int(audio.get("bitrate", 32000))}
    record = false
}}
""",
    }
    for name, body in files.items():
        with open(os.path.join(out, name), "w", encoding="utf-8") as f:
            f.write(body.lstrip())
    print(f"Janus 設定を生成しました: {out} (public_ip={public_ip or '未設定'}, rtp={rtp_range})")


if __name__ == "__main__":
    main()
