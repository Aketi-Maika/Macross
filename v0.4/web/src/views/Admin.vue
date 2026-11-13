<script setup>
import { ref, reactive, onMounted } from 'vue'
import { api, session } from '../api.js'

const channels = ref([])
const maxChannels = ref(9)
const maxConnections = ref(30)
const newChannel = ref({ name: '', max_speakers: 1 })
const users = ref([])
const error = ref('')
const notice = ref('')
const system = ref(null)
const SUPERUSER = '0000'
const isSuper = () => !!session.user?.superuser

/* ユーザごとの使えるチャンネル（編集中のユーザ ID → { all, ids }） */
const editing = reactive({})

function channelText(u) {
  if (u.admin) return 'すべて（admin）'
  if (!u.channels) return 'すべて'
  if (!u.channels.length) return 'なし'
  return u.channels.map((id) => channels.value.find((c) => c.id === id)?.name ?? `#${id}`).join('、')
}

function editChannels(u) {
  editing[u.id] = { all: !u.channels, ids: [...(u.channels ?? channels.value.map((c) => c.id))] }
}

async function saveChannels(u) {
  error.value = notice.value = ''
  const e = editing[u.id]
  try {
    await api.put(`/api/admin/users/${u.id}/channels`, { all: e.all, channels: e.all ? [] : e.ids })
    notice.value = `${u.name}（${u.id}）の使えるチャンネルを保存しました（接続中なら すぐに反映されます）`
    delete editing[u.id]
    await load()
  } catch (err) {
    error.value = err.message
  }
}

/** 他の admin の操作はスーパーユーザーだけ */
function canManage(u) {
  if (u.id === SUPERUSER || u.id === session.user?.id) return false
  return isSuper() || !u.admin
}

const form = ref({ name: '', furigana: '', email: '', send_mail: true })
const issued = ref(null)

/* メール設定 */
const mail = ref({ enabled: false, host: '', port: 587, security: 'starttls', username: '', password: '', from_address: '', from_name: 'Macross' })
const defaultFrom = ref('')
const hasPassword = ref(false)
const testTo = ref('')
const mailBusy = ref(false)
const mailEnabled = ref(false)
const PORTS = { starttls: 587, tls: 465, none: 25 }

/* CSV 一括登録 */
const importFile = ref(null)
const importMail = ref(true)
const imported = ref([])
const resetMail = ref(true)

function mailResult(m) {
  if (!m || m === 'skipped') return ''
  return m === 'sent' ? 'メール送信済み' : `メール未送信: ${m}`
}

async function loadMail() {
  const m = await api.get('/api/admin/mail')
  hasPassword.value = m.has_password
  mailEnabled.value = m.enabled
  defaultFrom.value = m.default_from
  mail.value = { ...m, password: '' }
}

async function saveMail() {
  error.value = notice.value = ''
  try {
    await api.put('/api/admin/mail', { ...mail.value, port: Number(mail.value.port), password: mail.value.password || null })
    notice.value = 'メール設定を保存しました'
    await loadMail()
  } catch (e) {
    error.value = e.message
  }
}

async function sendTest() {
  error.value = notice.value = ''
  mailBusy.value = true
  try {
    await api.post('/api/admin/mail/test', { to: testTo.value })
    notice.value = `${testTo.value} にテストメールを送りました`
  } catch (e) {
    error.value = e.message
  } finally {
    mailBusy.value = false
  }
}

async function importCsv() {
  error.value = notice.value = ''
  const file = importFile.value?.files?.[0]
  if (!file) {
    error.value = 'CSV ファイルを選んでください'
    return
  }
  try {
    const csv = await file.text()
    const r = await api.post('/api/admin/users/import', { csv, send_mail: importMail.value && mailEnabled.value })
    imported.value = r.users
    notice.value = `${r.users.length} 人を登録しました`
    importFile.value.value = ''
    await load()
  } catch (e) {
    error.value = e.message
  }
}

async function load() {
  error.value = ''
  try {
    await loadMail()
    const c = await api.get('/api/admin/channels')
    channels.value = c.channels.map((ch) => ({ ...ch, editName: ch.name, editMax: ch.max_speakers }))
    maxChannels.value = c.max_channels
    maxConnections.value = c.max_connections
    users.value = await api.get('/api/admin/users')
    system.value = await api.get('/api/admin/system').catch(() => null)
  } catch (e) {
    error.value = e.message
  }
}

async function addChannel() {
  error.value = notice.value = ''
  try {
    const c = await api.post('/api/admin/channels', {
      name: newChannel.value.name,
      max_speakers: Number(newChannel.value.max_speakers),
    })
    notice.value = `チャンネル ${c.name} を作成しました`
    newChannel.value = { name: '', max_speakers: 1 }
    await load()
  } catch (e) {
    error.value = e.message
  }
}

async function saveChannel(ch) {
  error.value = notice.value = ''
  try {
    await api.patch(`/api/admin/channels/${ch.id}`, { name: ch.editName, max_speakers: Number(ch.editMax) })
    notice.value = `チャンネル ${ch.editName} を保存しました`
    await load()
  } catch (e) {
    error.value = e.message
  }
}

async function deleteChannel(ch) {
  if (!confirm(`チャンネル ${ch.name} を削除しますか？
参加中の人は切り離されます（録音ファイルは残ります）。`)) return
  error.value = notice.value = ''
  try {
    await api.del(`/api/admin/channels/${ch.id}`)
    notice.value = `チャンネル ${ch.name} を削除しました`
    await load()
  } catch (e) {
    error.value = e.message
  }
}

async function createUser() {
  error.value = notice.value = ''
  try {
    const r = await api.post('/api/admin/users', {
      ...form.value,
      send_mail: form.value.send_mail && mailEnabled.value && !!form.value.email,
    })
    issued.value = { ...r, name: form.value.name }
    form.value = { name: '', furigana: '', email: '', send_mail: form.value.send_mail }
    await load()
  } catch (e) {
    error.value = e.message
  }
}

async function setFlag(u, key, value) {
  error.value = notice.value = ''
  try {
    await api.patch(`/api/admin/users/${u.id}`, { [key]: value })
    await load()
  } catch (e) {
    error.value = e.message
  }
}

/** 初期パスワードを表示中のユーザ */
const shown = reactive({})

async function resetPassword(u) {
  if (!confirm(`${u.name}（${u.id}）のパスワードをリセットしますか？\n新しい初期パスワードを発行し、ログイン中の場合はログアウトさせます。`)) return
  error.value = notice.value = ''
  try {
    const r = await api.post(`/api/admin/users/${u.id}/reset-password`, {
      send_mail: resetMail.value && mailEnabled.value && !!u.email,
    })
    const m = mailResult(r.mail)
    notice.value = `${u.name}（${u.id}）の新しい初期パスワード: ${r.initial_password}（${m || '本人に伝えてください'}。次回ログイン時に変更を求められます）`
    await load()
    shown[u.id] = true
  } catch (e) {
    error.value = e.message
  }
}

onMounted(load)
</script>

<template>
  <div class="container">
    <p v-if="error" class="error">{{ error }}</p>
    <p v-if="notice" class="notice">{{ notice }}</p>

    <div class="panel">
      <h2>チャンネル（{{ channels.length }} / {{ maxChannels }}）</h2>
      <p class="muted">発言枠 = そのチャンネルで同時に話せる人数（1〜{{ maxConnections }}）。録音はチャンネル名のフォルダに保存されます。</p>
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th>No.</th>
              <th>名前</th>
              <th>発言枠</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="ch in channels" :key="ch.id">
              <td>{{ ch.id }}</td>
              <td><input v-model="ch.editName" maxlength="20" /></td>
              <td><input v-model="ch.editMax" type="number" min="1" :max="maxConnections" style="width: 80px" /></td>
              <td class="actions">
                <button
                  class="small"
                  :disabled="ch.editName === ch.name && Number(ch.editMax) === ch.max_speakers"
                  @click="saveChannel(ch)"
                >
                  保存
                </button>
                <button class="secondary small danger" :disabled="channels.length <= 1" @click="deleteChannel(ch)">削除</button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
      <form v-if="channels.length < maxChannels" class="row" style="margin-top: 12px" @submit.prevent="addChannel">
        <input v-model="newChannel.name" class="grow" maxlength="20" placeholder="新しいチャンネル名（例: ch2）" required />
        <input v-model="newChannel.max_speakers" type="number" min="1" :max="maxConnections" style="width: 80px" />
        <span>人</span>
        <button type="submit">追加</button>
      </form>
    </div>

    <div class="panel">
      <h2>ID 発行</h2>
      <form @submit.prevent="createUser">
        <div class="row">
          <div class="grow">
            <label>名前</label>
            <input v-model="form.name" required />
          </div>
          <div class="grow">
            <label>ふりがな</label>
            <input v-model="form.furigana" />
          </div>
        </div>
        <label>メールアドレス</label>
        <input v-model="form.email" type="email" />
        <label class="check">
          <input v-model="form.send_mail" type="checkbox" :disabled="!mailEnabled" />
          初期パスワードをメールで送る
          <span v-if="!mailEnabled" class="muted">（下の「メール設定」で有効にすると使えます）</span>
        </label>
        <div style="margin-top: 12px"><button type="submit">発行する</button></div>
      </form>
      <div v-if="issued" class="issued">
        {{ issued.name }} さんの ID <code>{{ issued.id }}</code> 初期パスワード
        <code>{{ issued.initial_password }}</code>
        <p v-if="issued.mail === 'sent'" class="muted">メールで送信しました。初回ログイン時に変更を求められます。</p>
        <p v-else-if="mailResult(issued.mail)" class="error">{{ mailResult(issued.mail) }}（本人に直接伝えてください）</p>
        <p v-else class="muted">本人に伝えてください。初回ログイン時に変更を求められます。</p>
      </div>
      <p class="muted">初期パスワードは本人が変更するまで、下のユーザ一覧の「表示」から確認できます。</p>
    </div>

    <div class="panel">
      <h2>CSV で一括登録</h2>
      <p class="muted">
        テンプレートに「名前・ふりがな・メールアドレス」を記入して読み込みます。ID と初期パスワードは自動で発行されます。
        「（例）」で始まる行は登録されません。1 行でも誤りがあると何も登録しません。
      </p>
      <div class="row">
        <a class="button-link" href="/api/admin/users/template.csv" download>テンプレートをダウンロード</a>
      </div>
      <form class="row" style="margin-top: 12px" @submit.prevent="importCsv">
        <input ref="importFile" type="file" accept=".csv,text/csv" class="grow" />
        <label class="check">
          <input v-model="importMail" type="checkbox" :disabled="!mailEnabled" />
          初期パスワードをメールで送る
        </label>
        <button type="submit">登録する</button>
      </form>
      <div v-if="imported.length" class="table-wrap" style="margin-top: 12px">
        <table>
          <thead>
            <tr><th>ID</th><th>名前</th><th>メール</th><th>初期パスワード</th><th>メール送信</th></tr>
          </thead>
          <tbody>
            <tr v-for="u in imported" :key="u.id">
              <td>{{ u.id }}</td>
              <td>{{ u.name }}</td>
              <td>{{ u.email }}</td>
              <td><code>{{ u.initial_password }}</code></td>
              <td :class="{ error: u.mail !== 'sent' && u.mail !== 'skipped' }">
                {{ u.mail === 'sent' ? '送信済み' : u.mail === 'skipped' ? '—' : u.mail }}
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>

    <div class="panel">
      <h2>ユーザ一覧</h2>
      <p class="muted">
        ID 0000 はスーパーユーザー（最上位。無効化・権限の解除はできません）。admin の付与・解除と、他の admin の無効化・パスワードリセットはスーパーユーザーだけができます。
        「使えるチャンネル」で一般ユーザが聴ける・話せるチャンネルを制限できます（admin は常にすべて）。
      </p>
      <label class="check">
        <input v-model="resetMail" type="checkbox" :disabled="!mailEnabled" />
        パスワードリセット時に新しい初期パスワードをメールで送る（メールアドレス登録済みの人のみ）
      </label>
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th>ID</th>
              <th>名前</th>
              <th>メール</th>
              <th>状態</th>
              <th>初期パスワード</th>
              <th>使えるチャンネル</th>
              <th>admin</th>
              <th>無効化</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="u in users" :key="u.id">
              <td>{{ u.id }}</td>
              <td>
                {{ u.name }}
                <span v-if="u.superuser" class="tag su">スーパーユーザー</span>
                <br /><span class="muted">{{ u.furigana }}</span>
              </td>
              <td>{{ u.email }}</td>
              <td>
                <span v-if="u.disabled" class="muted">無効</span>
                <span v-else-if="u.must_change" class="muted">初期パスワード</span>
                <span v-else>有効</span>
              </td>
              <td class="nowrap">
                <template v-if="u.initial_password">
                  <code v-if="shown[u.id]">{{ u.initial_password }}</code>
                  <span v-else class="muted">••••••••••</span>
                  <button class="secondary small" @click="shown[u.id] = !shown[u.id]">
                    {{ shown[u.id] ? '隠す' : '表示' }}
                  </button>
                </template>
                <span v-else class="muted">変更済み</span>
              </td>
              <td class="ch-cell">
                <template v-if="editing[u.id]">
                  <label class="check">
                    <input v-model="editing[u.id].all" type="checkbox" />
                    すべて（今後追加するチャンネルも）
                  </label>
                  <div v-if="!editing[u.id].all" class="ch-list">
                    <label v-for="c in channels" :key="c.id" class="check">
                      <input v-model="editing[u.id].ids" type="checkbox" :value="c.id" />
                      {{ c.name }}
                    </label>
                  </div>
                  <div class="row" style="margin-top: 6px">
                    <button class="small" @click="saveChannels(u)">保存</button>
                    <button class="secondary small" @click="delete editing[u.id]">取消</button>
                  </div>
                </template>
                <template v-else>
                  <span :class="{ muted: u.admin }">{{ channelText(u) }}</span>
                  <button v-if="!u.admin" class="secondary small" @click="editChannels(u)">変更</button>
                </template>
              </td>
              <td>
                <input
                  type="checkbox"
                  :checked="u.admin"
                  :disabled="!isSuper() || u.id === SUPERUSER || u.id === session.user?.id"
                  :title="isSuper() ? '' : 'admin の付与・解除はスーパーユーザー（0000）だけができます'"
                  @change="setFlag(u, 'admin', $event.target.checked)"
                />
              </td>
              <td>
                <input
                  type="checkbox"
                  :checked="u.disabled"
                  :disabled="!canManage(u)"
                  @change="setFlag(u, 'disabled', $event.target.checked)"
                />
              </td>
              <td>
                <button
                  class="secondary small"
                  :disabled="!canManage(u)"
                  :title="
                    u.id === session.user?.id
                      ? '自分のパスワードは「パスワード変更」から変更してください'
                      : u.id === SUPERUSER
                        ? '0000 のパスワードは管理画面からはリセットできません'
                        : !canManage(u)
                          ? '他の admin の操作はスーパーユーザーだけができます'
                          : ''
                  "
                  @click="resetPassword(u)"
                >
                  パスワードリセット
                </button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>

    <div class="panel">
      <h2>メール設定</h2>
      <p class="muted">
        初期パスワードの送信に使う SMTP サーバ。自宅・社内回線からは 25 番ポートで直接送れないことが多いため、プロバイダや
        Gmail（アプリパスワード）・Microsoft 365 などの送信サーバ（587 / 465）を指定してください。
      </p>
      <form @submit.prevent="saveMail">
        <label class="check"><input v-model="mail.enabled" type="checkbox" /> メール送信を有効にする</label>
        <div class="row">
          <div class="grow">
            <label>SMTP サーバ</label>
            <input v-model="mail.host" placeholder="smtp.example.com" />
          </div>
          <div>
            <label>暗号化</label>
            <select v-model="mail.security" @change="mail.port = PORTS[mail.security]">
              <option value="starttls">STARTTLS（587）</option>
              <option value="tls">SSL/TLS（465）</option>
              <option value="none">なし（LAN 内の中継のみ）</option>
            </select>
          </div>
          <div>
            <label>ポート</label>
            <input v-model="mail.port" type="number" min="1" max="65535" style="width: 100px" />
          </div>
        </div>
        <div class="row">
          <div class="grow">
            <label>ユーザ名</label>
            <input v-model="mail.username" autocomplete="off" />
          </div>
          <div class="grow">
            <label>パスワード</label>
            <input
              v-model="mail.password"
              type="password"
              autocomplete="new-password"
              :placeholder="hasPassword ? '保存済み（変更する場合のみ入力）' : ''"
            />
          </div>
        </div>
        <div class="row">
          <div class="grow">
            <label>送信元アドレス</label>
            <input v-model="mail.from_address" type="email" :placeholder="defaultFrom" />
          </div>
          <div class="grow">
            <label>送信者名</label>
            <input v-model="mail.from_name" />
          </div>
        </div>
        <div style="margin-top: 12px"><button type="submit">保存</button></div>
      </form>
      <form class="row" style="margin-top: 16px" @submit.prevent="sendTest">
        <input v-model="testTo" type="email" class="grow" placeholder="テストメールの宛先" required />
        <button type="submit" class="secondary" :disabled="!mailEnabled || mailBusy">
          {{ mailBusy ? '送信中…' : 'テスト送信' }}
        </button>
      </form>
      <p v-if="!mailEnabled" class="muted">保存してメール送信を有効にすると、テスト送信できます。</p>
      <p class="muted">
        送信元アドレスを空にすると既定の <code>{{ defaultFrom }}</code> を使います。SMTP サーバがこのアドレスからの送信を許可している必要があります（迷惑メール扱いを避けるため、ドメインの SPF / DKIM の設定も推奨）。
      </p>
    </div>

    <div v-if="system" class="panel">
      <h2>サーバ</h2>
      <p>
        設定ファイル: <code>{{ system.config_path }}</code>
      </p>
      <p class="muted">
        ドメイン・IP・録音の保存先・読み上げ・位置情報などはこのファイルで設定します。編集したらサーバで
        <code>cd /opt/macross && docker compose down && docker compose up -d</code> を実行してください。バージョン {{ system.version }}
      </p>
    </div>
  </div>
</template>

<style scoped>
.issued {
  margin-top: 12px;
  padding: 12px;
  border-radius: 8px;
  background: var(--bg);
  border: 1px solid var(--ok);
}
td input[type='checkbox'] { width: auto; }
.check { display: flex; align-items: center; gap: 6px; margin-top: 10px; }
.check input { width: auto; }
.button-link {
  display: inline-block;
  padding: 8px 14px;
  border-radius: 8px;
  border: 1px solid var(--line);
  text-decoration: none;
}
.actions, .nowrap { white-space: nowrap; }
.nowrap button { margin-left: 6px; }
.actions button + button { margin-left: 6px; }
.small { padding: 6px 10px; font-size: .8rem; }
.tag { font-size: .7rem; padding: 1px 6px; margin-left: 4px; border-radius: 4px; background: var(--warn); color: #000; }
.tag.su { background: #b18cff; }
.ch-cell { min-width: 160px; }
.ch-cell > button { margin-left: 6px; }
.ch-cell .check { margin-top: 4px; font-size: .85rem; color: var(--text); }
.ch-list { padding-left: 8px; }
.danger { color: var(--talk); }
</style>
