<script setup>
import { ref, reactive, computed, onMounted } from 'vue'
import { api, session } from '../api.js'
import UiIcon from '../components/UiIcon.vue'
import ChannelBadge from '../components/ChannelBadge.vue'

const activeTab = ref('channels')
const tabs = [
  { id: 'channels', label: 'チャンネル', icon: 'radio' },
  { id: 'users', label: 'ユーザー', icon: 'user' },
  { id: 'mail', label: 'メール設定', icon: 'mail' },
  { id: 'server', label: 'サーバ', icon: 'server' },
]
function tabKey(event, index) {
  const nextIndex = event.key === 'ArrowRight' ? (index + 1) % tabs.length : event.key === 'ArrowLeft' ? (index + tabs.length - 1) % tabs.length : event.key === 'Home' ? 0 : event.key === 'End' ? tabs.length - 1 : -1
  if (nextIndex < 0) return
  event.preventDefault()
  activeTab.value = tabs[nextIndex].id
  event.currentTarget.parentElement.children[nextIndex].focus()
}

const channels = ref([])
const maxChannels = ref(9)
const maxConnections = ref(30)
const newChannel = ref({ name: '', max_speakers: 1 })
const users = ref([])

/* ユーザー一覧の検索（名前・ふりがな・ID・メールアドレス。全角半角・カタカナひらがな・空白・大文字小文字を区別しない） */
const userQuery = ref('')
function normalize(t) {
  return String(t ?? '')
    .normalize('NFKC')
    .toLowerCase()
    .replace(/[ァ-ヶ]/g, (c) => String.fromCharCode(c.charCodeAt(0) - 0x60))
    .replace(/\s+/g, '')
}
const filteredUsers = computed(() => {
  const words = userQuery.value.normalize('NFKC').split(/\s+/).map(normalize).filter(Boolean)
  if (!words.length) return users.value
  return users.value.filter((u) => {
    const hay = normalize([u.display_id, Number(u.display_id), u.name, u.furigana, u.email, showInternal.value ? u.uid : ''].join(' '))
    return words.every((w) => hay.includes(w))
  })
})
const error = ref('')
const notice = ref('')
const system = ref(null)
const isSuper = () => !!session.user?.superuser

/* 内部用 ID の表示（ボタンで切り替え。既定は隠す） */
const showInternal = ref(false)

/* 一括操作: 選んだユーザー（内部用 ID → true） */
const selected = reactive({})
const selectable = (u) => !u.superuser && u.id !== session.user?.id
const selectedUsers = computed(() => users.value.filter((u) => selected[u.id]))
const allShownSelected = computed(() => {
  const list = filteredUsers.value.filter(selectable)
  return list.length > 0 && list.every((u) => selected[u.id])
})
function toggleAll(on) {
  for (const u of filteredUsers.value.filter(selectable)) {
    if (on) selected[u.id] = true
    else delete selected[u.id]
  }
}
function clearSelection() {
  for (const k of Object.keys(selected)) delete selected[k]
}
const bulkBusy = ref(false)
const bulkChannels = ref({ open: false, all: true, ids: [] })
const bulkResult = ref(null)
const ACTION_LABEL = {
  delete: '削除',
  disable: '無効化',
  enable: '有効化',
  reset: 'パスワードリセット',
  admin_on: '管理者の付与',
  admin_off: '管理者の解除',
  channels: '使えるチャンネルの設定',
}

async function bulk(action) {
  const list = selectedUsers.value
  if (!list.length) return
  const names = list.slice(0, 8).map((u) => `${u.display_id} ${u.name}`).join('\n') + (list.length > 8 ? `\n…ほか ${list.length - 8} 人` : '')
  const extra = {
    delete: '\n\n削除すると元に戻せません（録音・位置の記録は残ります）。ログイン中の人は切断されます。\n表示用 ID の欠番は「番号を詰める」で付け直せます。',
    disable: '\n\nログイン中の人は切断されます。',
    reset: '\n\n新しい初期パスワードを発行し、ログイン中の人はログアウトさせます。',
  }[action] ?? ''
  if (!confirm(`${list.length} 人を${ACTION_LABEL[action]}しますか？\n\n${names}${extra}`)) return
  if (action === 'delete' && list.length >= 5 && prompt(`確認のため、削除する人数（${list.length}）を入力してください`) !== String(list.length)) return
  error.value = notice.value = ''
  bulkBusy.value = true
  try {
    const body = { ids: list.map((u) => u.id), action }
    if (action === 'reset') body.send_mail = resetMail.value && mailEnabled.value
    if (action === 'channels') body.channels = bulkChannels.value.all ? null : bulkChannels.value.ids
    const r = await api.post('/api/admin/users/bulk', body)
    const skipped = r.results.filter((x) => x.result !== 'ok')
    notice.value = `${r.done} 人を${ACTION_LABEL[action]}しました` + (skipped.length ? `（対象外 ${skipped.length} 人。下の結果を確認してください）` : '')
    bulkResult.value = action === 'reset' || skipped.length ? { action, results: r.results } : null
    if (action === 'reset') for (const x of r.results) if (x.initial_password) shown[x.id] = true
    bulkChannels.value.open = false
    clearSelection()
  } catch (e) {
    error.value = e.message
  } finally {
    bulkBusy.value = false
    await load()
  }
}

/* 表示用 ID の欠番（削除でできる）。ボタンで 1, 2, 3… に詰め直す */
const gaps = computed(() => {
  const max = Math.max(0, ...users.value.map((u) => Number(u.display_id) || 0))
  return Math.max(0, max + 1 - users.value.length)
})
const renumbered = ref([])
async function renumber() {
  if (!confirm(`表示用 ID を 1, 2, 3… の連番に詰め直しますか？（欠番 ${gaps.value}）\n\n欠番より後ろの人はログイン ID が変わります（パスワード・記録はそのまま）。\n変わった人の一覧を表示しますので、本人に新しい ID を伝えてください。`)) return
  error.value = notice.value = ''
  try {
    const r = await api.post('/api/admin/users/renumber', {})
    renumbered.value = r.changed
    notice.value = r.changed.length ? `${r.changed.length} 人の ID を付け直しました（下の一覧を確認してください）` : '欠番はありません'
    await load()
  } catch (e) {
    error.value = e.message
  }
}

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
    notice.value = `${u.name}（${u.display_id}）の使えるチャンネルを保存しました（接続中なら すぐに反映されます）`
    delete editing[u.id]
    await load()
  } catch (err) {
    error.value = err.message
  }
}

/* 名前・メールアドレスの編集（編集中のユーザ ID → { name, furigana, email }） */
const renaming = reactive({})

/** 名前を変えられるか: 自分・一般ユーザは admin、スーパーユーザーと他の admin はスーパーユーザーだけ */
function canRename(u) {
  if (u.id === session.user?.id) return true
  if (u.superuser) return isSuper()
  return isSuper() || !u.admin
}

function editName(u) {
  renaming[u.id] = { name: u.name, furigana: u.furigana ?? '', email: u.email ?? '' }
}

async function saveName(u) {
  error.value = notice.value = ''
  const r = renaming[u.id]
  if (!r.name.trim()) {
    error.value = '名前を入力してください'
    return
  }
  try {
    const res = await api.put(`/api/admin/users/${u.id}/profile`, { name: r.name, furigana: r.furigana, email: r.email })
    const mailChanged = (u.email ?? '') !== res.email
    notice.value = `${u.display_id} の${mailChanged ? '名前・メールアドレス' : '名前'}を保存しました（${res.name}${res.email ? ' / ' + res.email : ''}）`
    delete renaming[u.id]
    if (u.id === session.user?.id && session.user) session.user.name = res.name
    await load()
  } catch (err) {
    error.value = err.message
  }
}

/** 他の admin の操作はスーパーユーザーだけ */
function canManage(u) {
  if (u.superuser || u.id === session.user?.id) return false
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
const mailInfo = ref({ builtin: false, builtin_available: false, builtin_host: '', builtin_username: '' })
const PORTS = { starttls: 587, tls: 465, none: 25 }

/* CSV 一括登録 */
const importFile = ref(null)
const importMail = ref(true)
const imported = ref([])
const importBusy = ref(false)

/* CSV を文字列にする。UTF-8 で読めなければ Excel の既定（Shift_JIS）として読む */
async function readCsv(file) {
  const buf = await file.arrayBuffer()
  try {
    return new TextDecoder('utf-8', { fatal: true }).decode(buf)
  } catch {
    return new TextDecoder('shift_jis').decode(buf)
  }
}
const resetMail = ref(true)

function mailResult(m) {
  if (!m || m === 'skipped') return ''
  return m === 'sent' ? 'メール送信済み' : `メール未送信: ${m}`
}

async function loadMail() {
  const m = await api.get('/api/admin/mail')
  hasPassword.value = m.has_password
  mailEnabled.value = m.active ?? m.enabled
  mailInfo.value = m
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
  if (importBusy.value) return
  importBusy.value = true
  const sendMail = importMail.value && mailEnabled.value
  notice.value = sendMail ? '登録してメールを送っています…（人数が多いと時間がかかります。この画面のままお待ちください）' : '登録しています…'
  try {
    const csv = await readCsv(file)
    const r = await api.post('/api/admin/users/import', { csv, send_mail: sendMail })
    imported.value = r.users
    const failed = r.users.filter((u) => u.mail !== 'sent' && u.mail !== 'skipped').length
    notice.value = `${r.users.length} 人を登録しました（${r.users[0]?.display_id}〜${r.users[r.users.length - 1]?.display_id}）` + (failed ? `。メールを送れなかった人が ${failed} 人います（下の表を確認してください）` : '')
    importFile.value.value = ''
  } catch (e) {
    notice.value = ''
    error.value = e.message
  } finally {
    importBusy.value = false
    await load()
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
  if (!confirm(`${u.name}（${u.display_id}）のパスワードをリセットしますか？\n新しい初期パスワードを発行し、ログイン中の場合はログアウトさせます。`)) return
  error.value = notice.value = ''
  try {
    const r = await api.post(`/api/admin/users/${u.id}/reset-password`, {
      send_mail: resetMail.value && mailEnabled.value && !!u.email,
    })
    const m = mailResult(r.mail)
    notice.value = `${u.name}（${u.display_id}）の新しい初期パスワード: ${r.initial_password}（${m || '本人に伝えてください'}。次回ログイン時に変更を求められます）`
    await load()
    shown[u.id] = true
  } catch (e) {
    error.value = e.message
  }
}

onMounted(load)
</script>

<template>
  <div class="container admin-page" :class="'tab-' + activeTab">
    <nav class="admin-tabs panel" role="tablist" aria-label="管理メニュー">
      <button v-for="(tab, index) in tabs" :id="'tab-' + tab.id" :key="tab.id" type="button" role="tab" :aria-selected="activeTab === tab.id" :aria-controls="'panel-' + tab.id" :tabindex="activeTab === tab.id ? 0 : -1" :class="{ active: activeTab === tab.id }" @click="activeTab = tab.id" @keydown="tabKey($event, index)"><UiIcon :name="tab.icon" />{{ tab.label }}</button>
    </nav>
    <p v-if="error" class="error admin-message" role="alert">{{ error }}</p>
    <p v-if="notice" class="notice admin-message" role="status">{{ notice }}</p>

    <div v-show="activeTab === 'channels'" id="panel-channels" class="panel channels-panel" role="tabpanel" aria-labelledby="tab-channels">
      <h2>チャンネル（{{ channels.length }} / {{ maxChannels }}）</h2>
      <p class="muted">発言枠 = そのチャンネルで同時に話せる人数（1〜{{ maxConnections }}）。録音はチャンネル名のフォルダに保存されます。</p>
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th>No.</th>
              <th>表示</th>
              <th>名前</th>
              <th>発言枠</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="ch in channels" :key="ch.id">
              <td>{{ ch.id }}</td>
              <td><ChannelBadge :id="ch.id" /></td>
              <td><input v-model="ch.editName" :aria-label="`チャンネル ${ch.id} の名前`" maxlength="20" /></td>
              <td><input v-model="ch.editMax" :aria-label="`チャンネル ${ch.id} の発言枠`" type="number" min="1" :max="maxConnections" style="width: 80px" /></td>
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
        <input v-model="newChannel.name" class="grow" maxlength="20" placeholder="新しいチャンネル名" aria-label="新しいチャンネル名" required />
        <input v-model="newChannel.max_speakers" type="number" min="1" :max="maxConnections" style="width: 80px" aria-label="新しいチャンネルの発言枠" />
        <span>人</span>
        <button type="submit">追加</button>
      </form>
    </div>

    <!-- 右列（ID 発行・CSV）をまとめて上に詰める。ユーザー一覧が長くても CSV 欄が下にずれない -->
    <div v-show="activeTab === 'users'" class="users-side">
    <div v-show="activeTab === 'users'" class="panel issue-panel">
      <h2>ID 発行</h2>
      <form @submit.prevent="createUser">
        <div class="row">
          <div class="grow">
            <label for="new-user-name">名前</label>
            <input id="new-user-name" v-model="form.name" required />
          </div>
          <div class="grow">
            <label for="new-user-furigana">ふりがな</label>
            <input id="new-user-furigana" v-model="form.furigana" />
          </div>
        </div>
        <label for="new-user-email">メールアドレス</label>
        <input id="new-user-email" v-model="form.email" type="email" />
        <label class="check">
          <input v-model="form.send_mail" type="checkbox" :disabled="!mailEnabled" />
          初期パスワードをメールで送る
          <span v-if="!mailEnabled" class="muted">（「メール設定」タブで有効にすると使えます）</span>
        </label>
        <div style="margin-top: 12px"><button type="submit">発行する</button></div>
      </form>
      <div v-if="issued" class="issued">
        {{ issued.name }} さんの ID <code>{{ issued.display_id }}</code> 初期パスワード
        <code>{{ issued.initial_password }}</code>
        <p v-if="issued.mail === 'sent'" class="muted">メールで送信しました。初回ログイン時に変更を求められます。</p>
        <p v-else-if="mailResult(issued.mail)" class="error">{{ mailResult(issued.mail) }}（本人に直接伝えてください）</p>
        <p v-else class="muted">本人に伝えてください。初回ログイン時に変更を求められます。</p>
      </div>
      <p class="muted">初期パスワードは本人が変更するまで、ユーザー一覧の「表示」から確認できます。</p>
    </div>

    <div v-show="activeTab === 'users'" class="panel import-panel">
      <h2>CSV で一括登録</h2>
      <p class="muted">
        テンプレートに「名前・ふりがな・メールアドレス」を記入して読み込みます。ID と初期パスワードは自動で発行されます。
        「（例）」で始まる行は登録されません。人数の上限はなく、全行に ID を発行します。1 行でも誤りがあると何も登録しません。
      </p>
      <div class="row">
        <a class="button-link outline" href="/api/admin/users/template.csv" download><UiIcon name="download" />テンプレートをダウンロード</a>
      </div>
      <form class="row" style="margin-top: 12px" @submit.prevent="importCsv">
        <input ref="importFile" type="file" accept=".csv,text/csv" class="grow" aria-label="登録するCSVファイル" />
        <label class="check">
          <input v-model="importMail" type="checkbox" :disabled="!mailEnabled" />
          初期パスワードをメールで送る
        </label>
        <button type="submit" :disabled="importBusy">{{ importBusy ? '登録中…' : '登録する' }}</button>
      </form>
      <div v-if="imported.length" class="table-wrap" style="margin-top: 12px">
        <table>
          <thead>
            <tr><th>ID</th><th>名前</th><th>メール</th><th>初期パスワード</th><th>メール送信</th></tr>
          </thead>
          <tbody>
            <tr v-for="u in imported" :key="u.id">
              <td>{{ u.display_id }}</td>
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
    </div>

    <div v-show="activeTab === 'users'" id="panel-users" class="panel users-panel" role="tabpanel" aria-labelledby="tab-users">
      <h2>ユーザー一覧</h2>
      <p class="muted">
        ID 00000000 はスーパーユーザー（最上位。無効化・権限の解除・削除はできません）。左のチェックで選んだ人に、削除・無効化などをまとめて行えます。admin の付与・解除と、他の admin の無効化・パスワードリセットはスーパーユーザーだけができます。
        「使えるチャンネル」で一般ユーザが聴ける・話せるチャンネルを制限できます（admin は常にすべて）。
        名前の横の「変更」で名前・ふりがな・メールアドレスを変えられます（録音の記録には、話した時点の名前が残ります）。
      </p>
      <label class="check">
        <input v-model="resetMail" type="checkbox" :disabled="!mailEnabled" />
        パスワードリセット時に新しい初期パスワードをメールで送る（メールアドレス登録済みの人のみ）
      </label>
      <div class="user-search">
        <input v-model="userQuery" type="search" placeholder="名前・ふりがな・ID・メールで検索" aria-label="ユーザーを検索" />
        <span class="muted">{{ userQuery.trim() ? `${filteredUsers.length} / ${users.length} 人` : `${users.length} 人` }}</span>
        <span class="grow"></span>
        <button type="button" class="secondary small" :aria-pressed="showInternal" @click="showInternal = !showInternal">
          {{ showInternal ? '内部 ID を隠す' : '内部 ID を表示' }}
        </button>
        <button v-if="gaps" type="button" class="secondary small" title="削除でできた欠番を詰めて、表示用 ID を 1, 2, 3… の連番にします" @click="renumber">
          番号を詰める（欠番 {{ gaps }}）
        </button>
      </div>
      <div v-if="selectedUsers.length" class="bulk-bar" role="region" aria-label="選択したユーザーへの一括操作">
        <strong>{{ selectedUsers.length }} 人を選択中</strong>
        <button type="button" class="secondary small" :disabled="bulkBusy" @click="bulk('disable')">無効化</button>
        <button type="button" class="secondary small" :disabled="bulkBusy" @click="bulk('enable')">有効化</button>
        <button type="button" class="secondary small" :disabled="bulkBusy" @click="bulk('reset')">パスワードリセット</button>
        <button type="button" class="secondary small" :disabled="bulkBusy" :aria-expanded="bulkChannels.open" @click="bulkChannels.open = !bulkChannels.open">使えるチャンネル…</button>
        <template v-if="isSuper()">
          <button type="button" class="secondary small" :disabled="bulkBusy" @click="bulk('admin_on')">管理者にする</button>
          <button type="button" class="secondary small" :disabled="bulkBusy" @click="bulk('admin_off')">管理者を解除</button>
        </template>
        <button type="button" class="small danger-fill" :disabled="bulkBusy" @click="bulk('delete')">削除</button>
        <button type="button" class="secondary small" :disabled="bulkBusy" @click="clearSelection">選択を解除</button>
        <span v-if="bulkBusy" class="muted">処理中…</span>
        <div v-if="bulkChannels.open" class="bulk-channels">
          <label class="check"><input v-model="bulkChannels.all" type="checkbox" /> すべて（今後追加するチャンネルも）</label>
          <template v-if="!bulkChannels.all">
            <label v-for="c in channels" :key="c.id" class="check"><input v-model="bulkChannels.ids" type="checkbox" :value="c.id" /> {{ c.name }}</label>
          </template>
          <button type="button" class="small" :disabled="bulkBusy" @click="bulk('channels')">選んだ人に設定</button>
        </div>
      </div>
      <div v-if="bulkResult" class="table-wrap bulk-result">
        <div class="row"><strong>{{ ACTION_LABEL[bulkResult.action] }}の結果</strong><button type="button" class="secondary small" @click="bulkResult = null">閉じる</button></div>
        <table>
          <thead><tr><th>ID</th><th>名前</th><th>結果</th><th v-if="bulkResult.action === 'reset'">新しい初期パスワード</th><th v-if="bulkResult.action === 'reset'">メール</th></tr></thead>
          <tbody>
            <tr v-for="x in bulkResult.results" :key="x.id">
              <td>{{ x.display_id }}</td>
              <td>{{ x.name }}</td>
              <td :class="{ error: x.result !== 'ok' }">{{ x.result === 'ok' ? '完了' : x.result }}</td>
              <td v-if="bulkResult.action === 'reset'"><code v-if="x.initial_password">{{ x.initial_password }}</code></td>
              <td v-if="bulkResult.action === 'reset'" :class="{ error: x.mail && x.mail !== 'sent' && x.mail !== 'skipped' }">{{ x.mail === 'sent' ? '送信済み' : x.mail === 'skipped' || !x.mail ? '—' : x.mail }}</td>
            </tr>
          </tbody>
        </table>
      </div>
      <div v-if="renumbered.length" class="table-wrap bulk-result">
        <div class="row"><strong>ID が変わった人（本人に新しいログイン ID を伝えてください）</strong><button type="button" class="secondary small" @click="renumbered = []">閉じる</button></div>
        <table>
          <thead><tr><th>名前</th><th>旧 ID</th><th>新 ID</th></tr></thead>
          <tbody>
            <tr v-for="x in renumbered" :key="x.id"><td>{{ x.name }}</td><td>{{ x.old }}</td><td><strong>{{ x.new }}</strong></td></tr>
          </tbody>
        </table>
      </div>
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th class="sel-cell"><input type="checkbox" :checked="allShownSelected" aria-label="表示中のユーザーをすべて選択" @change="toggleAll($event.target.checked)" /></th>
              <th>ID</th>
              <th>名前</th>
              <th>状態</th>
              <th>初期パスワード</th>
              <th>使えるチャンネル</th>
              <th>管理者</th>
              <th>無効化</th>
              <th></th>
            </tr>
          </thead>
          <tbody>
            <tr v-if="!filteredUsers.length"><td colspan="9" class="muted">該当するユーザーはいません</td></tr>
            <tr v-for="u in filteredUsers" :key="u.id" :class="{ 'row-selected': selected[u.id] }">
              <td class="sel-cell">
                <input
                  type="checkbox"
                  :checked="!!selected[u.id]"
                  :disabled="!selectable(u)"
                  :aria-label="`${u.display_id} ${u.name} を選択`"
                  :title="u.superuser ? 'スーパーユーザーは対象外です' : u.id === session.user?.id ? '自分自身は対象外です' : ''"
                  @change="$event.target.checked ? (selected[u.id] = true) : delete selected[u.id]"
                />
              </td>
              <td class="id-cell">
                {{ u.display_id }}
                <template v-if="showInternal">
                  <br /><span class="muted internal-id" title="内部用 ID（16 進数 8 桁。変わりません）">内部 <code>{{ u.id }}</code></span>
                  <br /><span class="muted internal-id" title="[内部用 8 桁][表示用 8 桁]">{{ u.uid }}</span>
                </template>
              </td>
              <td class="name-cell">
                <form v-if="renaming[u.id]" class="name-edit" @submit.prevent="saveName(u)" @keydown.esc="delete renaming[u.id]">
                  <input v-model="renaming[u.id].name" maxlength="40" required placeholder="名前" :aria-label="`${u.display_id} の名前`" />
                  <input v-model="renaming[u.id].furigana" maxlength="40" placeholder="ふりがな" :aria-label="`${u.display_id} のふりがな`" />
                  <input v-model="renaming[u.id].email" type="email" maxlength="254" placeholder="メールアドレス（空欄可）" autocomplete="off" :aria-label="`${u.display_id} のメールアドレス`" />
                  <div class="row">
                    <button type="submit" class="small">保存</button>
                    <button type="button" class="secondary small" @click="delete renaming[u.id]">取消</button>
                  </div>
                </form>
                <template v-else>
                  {{ u.name }}
                  <span v-if="u.superuser" class="tag su">スーパーユーザー</span>
                  <button v-if="canRename(u)" type="button" class="secondary small rename-button" :aria-label="`${u.name} の名前・メールアドレスを変更`" title="名前・メールアドレスを変更" @click="editName(u)">変更</button>
                  <br /><span class="muted">{{ u.furigana }}</span>
                  <br v-if="u.email" /><span v-if="u.email" class="muted user-email">{{ u.email }}</span>
                </template>
              </td>
              <td>
                <span v-if="u.disabled" class="status-chip">無効</span>
                <span v-else-if="u.must_change" class="status-chip warning">初期パスワード</span>
                <span v-else class="status-chip success"><UiIcon name="check" />有効</span>
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
                  :aria-label="u.name + ' に管理者権限を付与'"
                  :disabled="!isSuper() || u.superuser || u.id === session.user?.id"
                  :title="isSuper() ? '' : 'admin の付与・解除はスーパーユーザー（00000000）だけができます'"
                  @change="setFlag(u, 'admin', $event.target.checked)"
                />
              </td>
              <td>
                <input
                  type="checkbox"
                  :checked="u.disabled"
                  :aria-label="u.name + ' を無効化'"
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
                      : u.superuser
                        ? 'スーパーユーザーのパスワードは管理画面からはリセットできません'
                        : !canManage(u)
                          ? '他の admin の操作はスーパーユーザーだけができます'
                          : ''
                  "
                  @click="resetPassword(u)"
                >
                  リセット
                </button>
              </td>
            </tr>
          </tbody>
        </table>
      </div>
    </div>

    <div v-show="activeTab === 'mail'" id="panel-mail" class="mail-layout" role="tabpanel" aria-labelledby="tab-mail">
      <section class="panel mail-panel">
      <h2>メール設定</h2>
      <p class="muted">
        初期パスワードの送信に使う SMTP サーバ。自宅・社内回線からは 25 番ポートで直接送れないことが多いため、プロバイダや
        Gmail（アプリパスワード）・Microsoft 365 などの送信サーバ（587 / 465）を指定してください。
      </p>
      <p v-if="mailInfo.builtin" class="notice builtin-mail">
        <UiIcon name="check" />既定のメールサーバ（さくら <code>{{ mailInfo.builtin_host }}</code> / <code>{{ mailInfo.builtin_username }}</code>）で送信します。設定は不要です。別のサーバを使う場合だけ、下で設定して「有効にする」をオンにしてください。
      </p>
      <p v-else-if="!mailInfo.builtin_available && !mailEnabled" class="muted">
        既定のメールサーバ（Macross.yaml の <code>mail.smtp_password</code>）が未設定です。サーバの設定ファイルに書くか、下で設定してください。
      </p>
      <form @submit.prevent="saveMail">
        <label class="check"><input v-model="mail.enabled" type="checkbox" /> {{ mailInfo.builtin_available ? '既定の代わりに下の設定で送信する' : 'メール送信を有効にする' }}</label>
        <div class="row">
          <div class="grow">
            <label for="smtp-host">SMTP サーバ</label>
            <input id="smtp-host" v-model="mail.host" placeholder="smtp.example.com" />
          </div>
          <div>
            <label for="smtp-security">暗号化</label>
            <select id="smtp-security" v-model="mail.security" @change="mail.port = PORTS[mail.security]">
              <option value="starttls">STARTTLS（587）</option>
              <option value="tls">SSL/TLS（465）</option>
              <option value="none">なし（LAN 内の中継のみ）</option>
            </select>
          </div>
          <div>
            <label for="smtp-port">ポート</label>
            <input id="smtp-port" v-model="mail.port" type="number" min="1" max="65535" style="width: 100px" />
          </div>
        </div>
        <div class="row">
          <div class="grow">
            <label for="smtp-user">ユーザ名</label>
            <input id="smtp-user" v-model="mail.username" autocomplete="off" />
          </div>
          <div class="grow">
            <label for="smtp-password">パスワード</label>
            <input
              id="smtp-password"
              v-model="mail.password"
              type="password"
              autocomplete="new-password"
              :placeholder="hasPassword ? '保存済み（変更する場合のみ入力）' : ''"
            />
          </div>
        </div>
        <div class="row">
          <div class="grow">
            <label for="smtp-from">送信元アドレス</label>
            <input id="smtp-from" v-model="mail.from_address" type="email" :placeholder="defaultFrom" />
          </div>
          <div class="grow">
            <label for="smtp-from-name">送信者名</label>
            <input id="smtp-from-name" v-model="mail.from_name" />
          </div>
        </div>
        <div style="margin-top: 12px"><button type="submit">保存</button></div>
      </form>
      <p class="muted">
        送信元アドレスを空にすると既定の <code>{{ defaultFrom }}</code> を使います。SMTP サーバがこのアドレスからの送信を許可している必要があります（迷惑メール扱いを避けるため、ドメインの SPF / DKIM の設定も推奨）。
      </p>
      </section>
      <aside class="panel mail-test-panel">
      <h2><UiIcon name="mail" />テスト送信</h2>
      <p class="muted">設定を保存したあと、メールが届くことを確認できます。</p>
      <form @submit.prevent="sendTest">
        <label for="smtp-test-to">テストメールの宛先</label>
        <input id="smtp-test-to" v-model="testTo" type="email" placeholder="name@example.com" required />
        <button type="submit" class="secondary" :disabled="!mailEnabled || mailBusy">
          {{ mailBusy ? '送信中…' : 'テスト送信' }}
        </button>
      </form>
      <p v-if="!mailEnabled" class="muted">メールサーバを設定すると、テスト送信できます。</p>
      </aside>
    </div>

    <div v-show="activeTab === 'server'" id="panel-server" class="panel server-panel" role="tabpanel" aria-labelledby="tab-server">
      <h2>サーバ情報 <span class="status-chip">参照のみ</span></h2>
      <template v-if="system">
      <p>
        設定ファイル: <code>{{ system.config_path }}</code>
      </p>
      <p class="muted">
        ドメイン・IP・録音の保存先・読み上げ・位置情報などはこのファイルで設定します。編集したらサーバで
        <code>cd /opt/macross && docker compose down && docker compose up -d</code> を実行してください。バージョン {{ system.version }}
      </p>
      </template>
      <p v-else class="muted">サーバ情報を取得できませんでした。</p>
    </div>
  </div>
</template>

<style scoped>
.admin-page { display: grid; gap: 20px; }
.admin-tabs { display: flex; align-items: center; flex-wrap: wrap; gap: 12px; padding: 12px 16px; grid-column: 1 / -1; }
.admin-tabs button { background: transparent; color: var(--text); padding: 10px 24px; }
.admin-tabs button.active { background: var(--accent); color: #fff; }
.admin-tabs button:hover:not(.active) { background: var(--soft); }
.admin-message { grid-column: 1 / -1; margin: 0; }
.tab-users { grid-template-columns: minmax(0, 2.3fr) minmax(300px, 1fr); align-items: start; }
.users-panel { grid-column: 1; order: 1; }
.users-side { grid-column: 2; order: 2; display: grid; gap: 20px; align-content: start; min-width: 0; }
.user-email { overflow-wrap: anywhere; }
.name-cell { min-width: 150px; }
.builtin-mail { display: flex; gap: 8px; align-items: flex-start; }
.rename-button { min-height: 28px; padding: 2px 8px; margin-left: 4px; font-size: .75rem; }
.name-edit { display: grid; gap: 6px; min-width: 180px; }
.name-edit input { min-height: 36px; padding: 6px 9px; }
.users-panel table { font-size: .85rem; }
.user-search { display: flex; gap: 12px; align-items: center; margin: 12px 0 4px; }
.user-search input { flex: 1; max-width: 360px; }
.user-search { flex-wrap: wrap; }
.user-search .grow { flex: 1; }
.bulk-bar { display: flex; flex-wrap: wrap; gap: 8px; align-items: center; margin: 8px 0; padding: 10px 12px; border: 1px solid var(--accent); border-radius: 10px; background: color-mix(in srgb, var(--accent) 8%, transparent); position: sticky; top: 0; z-index: 2; }
.bulk-channels { flex-basis: 100%; display: flex; flex-wrap: wrap; gap: 10px; align-items: center; padding-top: 6px; }
.danger-fill { background: #dc2626; border-color: #dc2626; color: #fff; }
.bulk-result { margin: 8px 0 12px; }
.bulk-result .row { justify-content: space-between; margin-bottom: 6px; }
.sel-cell { width: 36px; text-align: center; }
.row-selected td { background: color-mix(in srgb, var(--accent) 7%, transparent); }
.id-cell { white-space: nowrap; font-variant-numeric: tabular-nums; }
.internal-id { font-size: .78rem; }
.users-panel th, .users-panel td { padding: 12px 9px; }
.channels-panel input.grow { min-width: 200px; }
.channels-panel .channel-badge { width: 50px; height: 50px; }
.mail-layout { display: grid; grid-template-columns: minmax(0, 2.3fr) minmax(300px, 1fr); gap: 20px; align-items: start; }
.mail-test-panel button { width: 100%; margin-top: 16px; }
.server-panel { min-height: 300px; }
.issue-panel .row { display: block; }
.issue-panel button[type='submit'], .import-panel button[type='submit'], .import-panel .button-link { width: 100%; }
.import-panel .row { display: grid; width: 100%; }
@media (max-width: 1150px) { .tab-users, .mail-layout { grid-template-columns: minmax(0, 1fr); } .users-panel, .users-side { grid-column: 1; } }
@media (max-width: 700px) { .admin-tabs { gap: 4px; padding: 8px; } .admin-tabs button { flex: 1 1 40%; padding: 9px 8px; font-size: .9rem; } .mail-panel .row { display: block; } }
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
