# Macross要件定義書
## 正式名称
Made in Aketimaika Communication Radio with On-demand Speech Services
## Mission
- 可能な限り既存OSSを活用し構築を行う
- 低遅延/高画質な音声伝送を行う
- 自作の部分はRast/Vue.jsで作成を行う
- Serverの設定は[Macross.yaml]で管理を行う 
    - Server側での自動録音保存先
    - 接続上限
    - 通信品質
- Chromiumで動作可能
- 同時に喋れる人数をGUiから操作可能
- チャンネル(MAX9)分けられるように
- LRで異なるチャンネルを聴けるように設定
- 管理者権限をGUIから設定可能
- IDはCSVで管理(システムが自動発行)
- リアルタイム文字起こし(LLMをAPIで接続)
- [Macross.yaml]で指定した保存先にMP3で音声を保存
    - 音声ごとにタイムスタンプを記録
- 個人ごとにパスを任意に設定可能
    - 任意で2段階認証
- 発言ボタンが押されている間は録音

## 使用資源
- Rocky9
    - 4コア
    - 8GB
    - 64GB
- https://Macross.build-maika.jp
    - cloudFlareTunnel使用

## 目的/利用場面
- 無線を必要としているが、チャンネル不足/資金不足により準備ができない場合
- 盗聴が怖いが、無線が必用な場合

## 開発範囲・優先順位
1. 1チャンネルのみで音声伝送が可能
1. 9チャンネルまで対応
1. ２段階認証に対応
1. LLMに対応
1. k8sに対応
1. 最大接続数を300人に対応
1. デジタル簡易無線に接続可能に

## 音声伝送の経路
- テキスト入力による読み上げも行う
    - その時IDごとにユニークな音声にすること
- Serverは端末ごとの音声を受信し、Mixし、チャンネル受信者全員へは配付を行う
- Dockerコンテナでチャンネル管理を行い、マスターコンテナと必要に応じてサブコンテナを構築する
    - マスターコンテナのみで通信を支えることが難しくなった場合、サブコンテナを構築し、マスターはロード・バランシングを行なう
````mermaid
flowchart LR
A[ユーザー携帯]-->B
AA[PC]-->B
B[Chromiumブラウザ]--WebRTC-->C
C[Server]--WebRTC-->D[ユーザー携帯]
C--webRTC-->E[PC]
````

## ブラウザからの送信方式
- ユーザーはChromiumからURLにアクセスすることで利用可能にし、追加アプリケーションのインストールを不要にする
- ユーザーはチャンネルを選択し、相互に通信を行う
- 取得した音声はMP3で保存し、yamlで設定された任意の保存場所に保存する
- Grafanaで現在の通信量等を可視化する
- 可視化ツールは下記に対応して
    - Zabbix
    - Grafana
    - Prometheus
    - Loki

## 使用技術
- [RUST](https://rust-lang.org/ja/)
- [Vue.js](https://ja.vuejs.org/)
- [Docker](https://www.docker.com/ja-jp/)
- [OBS](https://obsproject.com/ja)
- [Zabbix](https://www.zabbix.com/jp)
- [Grafana](https://grafana.com/ja/)
- [Prometheus](https://prometheus.io/)
- [Loki](https://grafana.com/ja/oss/loki/)
- [WebRTC](https://imageflux.sakura.ad.jp/column/webrtc/?gad_source=1&gad_campaignid=24051150930&gclid=CjwKCAjw_pzWBhAkEiwAwDCi1XUJJzi0OO7wd0sfIlrUjS9kjmw7kzsMncnDSZgNAE5avQmRE5ZL9RoC9YUQAvD_BwE)
- [Cloudflare Tunnel](https://developers.cloudflare.com/tunnel/)

## 想定動作環境
## スマートフォン
- Chromium Webブラウザ
- Safari
## PC
- Chromium Webブラウザ
## Server
- LinuxServer[Rockyを作者はテスト予定]
    - 4コア以上
    - 8GB以上
    - 64GB以上