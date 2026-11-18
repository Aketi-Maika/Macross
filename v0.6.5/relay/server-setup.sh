#!/bin/sh
# Macross サーバ側の中継トンネル設定（Macross サーバ上で root で実行）
#
#   1) sh server-setup.sh key                      … 鍵を作り、公開鍵を表示（vps-setup.sh に渡す）
#   2) sh server-setup.sh up <VPS の IP> <VPS 公開鍵> … トンネルを張る
#
# 張ったあとは Macross.yaml の janus.public_ip を VPS の IP にし、DNS の A レコードも VPS の IP に向ける。
set -eu

umask 077
mkdir -p /etc/wireguard
KEY=/etc/wireguard/macross-relay.key

case "${1:-}" in
key)
  command -v wg >/dev/null || dnf install -y -q wireguard-tools >/dev/null || apt-get install -y -qq wireguard-tools >/dev/null
  [ -f "$KEY" ] || wg genkey > "$KEY"
  echo "このサーバの WireGuard 公開鍵（vps-setup.sh に渡す）:"
  wg pubkey < "$KEY"
  ;;
up)
  VPS_IP="${2:?VPS の IP を指定してください}"
  VPS_PUB="${3:?VPS の公開鍵を指定してください}"
  [ -f "$KEY" ] || { echo "先に「server-setup.sh key」を実行してください"; exit 1; }
  cat > /etc/wireguard/macross-relay.conf <<EOF
# Macross 中継トンネル（server-setup.sh が生成）
[Interface]
Address = 10.99.0.2/24
PrivateKey = $(cat "$KEY")

[Peer]
# 中継 VPS
PublicKey = $VPS_PUB
Endpoint = $VPS_IP:${WG_PORT:-31820}
AllowedIPs = 10.99.0.1/32
PersistentKeepalive = 25
EOF
  if command -v firewall-cmd >/dev/null && firewall-cmd --state >/dev/null 2>&1; then
    # トンネルから来る通信は public ゾーンと同じ扱い（80/443・UDP 20000-20200 は開放済み）
    firewall-cmd -q --permanent --zone=public --add-interface=macross-relay || true
    firewall-cmd -q --reload
  fi
  systemctl enable -q wg-quick@macross-relay
  systemctl restart wg-quick@macross-relay
  sleep 2
  if ping -c 2 -W 2 10.99.0.1 >/dev/null; then
    echo "トンネル接続 OK（10.99.0.2 ⇔ 10.99.0.1）"
  else
    echo "トンネルが繋がりません。VPS の UDP ${WG_PORT:-31820}（パケットフィルタ含む）が開いているか確認してください"; exit 1
  fi
  ;;
*)
  sed -n '2,9p' "$0"; exit 1
  ;;
esac
