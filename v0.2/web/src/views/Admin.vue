<script setup>
import { ref, onMounted } from 'vue'
import { api, session } from '../api.js'

const channels = ref([])
const maxChannels = ref(9)
const maxConnections = ref(30)
const newChannel = ref({ name: '', max_speakers: 1 })
const users = ref([])
const error = ref('')
const notice = ref('')

const form = ref({ name: '', furigana: '', email: '' })
const issued = ref(null)

async function load() {
  error.value = ''
  try {
    const c = await api.get('/api/admin/channels')
    channels.value = c.channels.map((ch) => ({ ...ch, editName: ch.name, editMax: ch.max_speakers }))
    maxChannels.value = c.max_channels
    maxConnections.value = c.max_connections
    users.value = await api.get('/api/admin/users')
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
    const r = await api.post('/api/admin/users', form.value)
    issued.value = { ...r, name: form.value.name }
    form.value = { name: '', furigana: '', email: '' }
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
        <div style="margin-top: 12px"><button type="submit">発行する</button></div>
      </form>
      <div v-if="issued" class="issued">
        {{ issued.name }} さんの ID <code>{{ issued.id }}</code> 初期パスワード
        <code>{{ issued.initial_password }}</code>
        <p class="muted">本人に伝えてください。初回ログイン時に変更を求められます。</p>
      </div>
    </div>

    <div class="panel">
      <h2>ユーザ一覧</h2>
      <div class="table-wrap">
        <table>
          <thead>
            <tr>
              <th>ID</th>
              <th>名前</th>
              <th>メール</th>
              <th>状態</th>
              <th>admin</th>
              <th>無効化</th>
            </tr>
          </thead>
          <tbody>
            <tr v-for="u in users" :key="u.id">
              <td>{{ u.id }}</td>
              <td>
                {{ u.name }}<br /><span class="muted">{{ u.furigana }}</span>
              </td>
              <td>{{ u.email }}</td>
              <td>
                <span v-if="u.disabled" class="muted">無効</span>
                <span v-else-if="u.must_change" class="muted">初期パスワード</span>
                <span v-else>有効</span>
              </td>
              <td>
                <input
                  type="checkbox"
                  :checked="u.admin"
                  :disabled="u.id === session.user?.id"
                  @change="setFlag(u, 'admin', $event.target.checked)"
                />
              </td>
              <td>
                <input
                  type="checkbox"
                  :checked="u.disabled"
                  :disabled="u.id === session.user?.id"
                  @change="setFlag(u, 'disabled', $event.target.checked)"
                />
              </td>
            </tr>
          </tbody>
        </table>
      </div>
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
.actions { white-space: nowrap; }
.actions button + button { margin-left: 6px; }
.small { padding: 6px 10px; font-size: .8rem; }
.danger { color: var(--talk); }
</style>
