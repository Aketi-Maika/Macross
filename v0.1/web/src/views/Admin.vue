<script setup>
import { ref, onMounted } from 'vue'
import { api, session } from '../api.js'

const maxSpeakers = ref(1)
const maxConnections = ref(30)
const users = ref([])
const error = ref('')
const notice = ref('')

const form = ref({ name: '', furigana: '', email: '' })
const issued = ref(null)

async function load() {
  error.value = ''
  try {
    const s = await api.get('/api/admin/settings')
    maxSpeakers.value = s.max_speakers
    maxConnections.value = s.max_connections
    users.value = await api.get('/api/admin/users')
  } catch (e) {
    error.value = e.message
  }
}

async function saveSettings() {
  error.value = notice.value = ''
  try {
    await api.put('/api/admin/settings', { max_speakers: Number(maxSpeakers.value) })
    notice.value = `発言枠を ${maxSpeakers.value} 人に変更しました`
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
      <h2>発言枠</h2>
      <p class="muted">同時に話せる人数（1〜{{ maxConnections }}）</p>
      <div class="row">
        <input v-model="maxSpeakers" type="number" min="1" :max="maxConnections" style="width: 100px" />
        <span>人</span>
        <button @click="saveSettings">保存</button>
      </div>
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
</style>
