#!/bin/sh
# Macross 中継 VPS のセットアップ（VPS 上で root で実行）
#
#   sh vps-setup.sh <Macross サーバの WireGuard 公開鍵>
#
# 外から VPS に届いた TCP 80/443 と UDP 20000-20200（音声）を、
# WireGuard トンネル（10.99.0.0/24）経由で Macross サーバ（10.99.0.2）へ転送する。
# 最後に VPS 側の公開鍵を表示するので、server-setup.sh に渡す。
set -eu

SERVER_PUB="${1:?Macross サーバの WireGuard 公開鍵を指定してください}"
WG_PORT="${WG_PORT:-31820}"   # さくらの VPS のパケットフィルタは 32767 までしか指定できないため 31820
RTP=20000:20200

# --- パッケージ
if command -v apt-get >/dev/null; then
  apt-get update -qq && DEBIAN_FRONTEND=noninteractive apt-get install -y -qq wireguard iptables >/dev/null
elif command -v dnf >/dev/null; then
  # 先に OS 全体を更新する（さくらの VPS の Rocky Linux 10.0 イメージで wireguard-tools だけ入れると
  # systemd だけが新しくなり OpenSSL と食い違って systemctl が動かなくなったため）
  dnf -y -q upgrade >/dev/null
  dnf install -y -q wireguard-tools iptables-nft >/dev/null || dnf install -y -q wireguard-tools iptables >/dev/null
  [ "$(rpm -q --last kernel | head -1 | awk '{print $1}')" = "kernel-$(uname -r)" ] || echo "※ カーネルが更新されました。設定後に再起動してください（sudo reboot）"
fi

EXT=$(ip route get 1.1.1.1 | awk '{for(i=1;i<=NF;i++) if($i=="dev") print $(i+1); exit}')
[ -n "$EXT" ] || { echo "外向きのインタフェースが分かりません"; exit 1; }

umask 077
mkdir -p /etc/wireguard
[ -f /etc/wireguard/macross.key ] || wg genkey > /etc/wireguard/macross.key
PRIV=$(cat /etc/wireguard/macross.key)
PUB=$(echo "$PRIV" | wg pubkey)

cat > /etc/wireguard/wg0.conf <<EOF
# Macross 中継（vps-setup.sh が生成）
[Interface]
Address = 10.99.0.1/24
ListenPort = $WG_PORT
PrivateKey = $PRIV
PostUp = sysctl -qw net.ipv4.ip_forward=1
PostUp = iptables -t nat -A PREROUTING -i $EXT -p tcp -m multiport --dports 80,443 -j DNAT --to-destination 10.99.0.2
PostUp = iptables -t nat -A PREROUTING -i $EXT -p udp --dport $RTP -j DNAT --to-destination 10.99.0.2
PostUp = iptables -t nat -A POSTROUTING -o wg0 -j MASQUERADE
PostUp = iptables -I FORWARD -i $EXT -o wg0 -j ACCEPT
PostUp = iptables -I FORWARD -i wg0 -o $EXT -m state --state RELATED,ESTABLISHED -j ACCEPT
PostDown = iptables -t nat -D PREROUTING -i $EXT -p tcp -m multiport --dports 80,443 -j DNAT --to-destination 10.99.0.2
PostDown = iptables -t nat -D PREROUTING -i $EXT -p udp --dport $RTP -j DNAT --to-destination 10.99.0.2
PostDown = iptables -t nat -D POSTROUTING -o wg0 -j MASQUERADE
PostDown = iptables -D FORWARD -i $EXT -o wg0 -j ACCEPT
PostDown = iptables -D FORWARD -i wg0 -o $EXT -m state --state RELATED,ESTABLISHED -j ACCEPT

[Peer]
# Macross サーバ
PublicKey = $SERVER_PUB
AllowedIPs = 10.99.0.2/32
EOF

# --- VPS 自身のファイアウォール
if command -v ufw >/dev/null && ufw status | grep -q active; then
  ufw allow $WG_PORT/udp; ufw allow 80/tcp; ufw allow 443/tcp; ufw allow $RTP/udp
  ufw route allow in on "$EXT" out on wg0
elif command -v firewall-cmd >/dev/null && firewall-cmd --state >/dev/null 2>&1; then
  firewall-cmd -q --permanent --add-port=$WG_PORT/udp --add-port=80/tcp --add-port=443/tcp --add-port=20000-20200/udp
  firewall-cmd -q --permanent --add-masquerade
  firewall-cmd -q --permanent --zone=trusted --add-interface=wg0
  firewall-cmd -q --reload
fi

systemctl enable -q wg-quick@wg0
systemctl restart wg-quick@wg0
echo
echo "VPS の公開 IP:      $(curl -4 -s -m 5 https://ifconfig.me || echo '?')"
echo "VPS の WireGuard 公開鍵（server-setup.sh に渡す）:"
echo "$PUB"
