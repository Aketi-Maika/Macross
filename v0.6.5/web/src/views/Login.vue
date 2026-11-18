<script setup>
import { ref, onMounted } from 'vue'
import { useRouter } from 'vue-router'
import { api, session } from '../api.js'
import UiIcon from '../components/UiIcon.vue'

const router = useRouter()
const id = ref('')
const password = ref('')
const error = ref('')
const busy = ref(false)
const showPassword = ref(false)
/** デモ: 一般ユーザーはパスワードなしで入室できる（v0.6.5。Macross.yaml の demo.open_entry） */
const openEntry = ref(false)
const guestName = ref('')

onMounted(async () => {
  try {
    openEntry.value = !!(await api.get('/api/config')).open_entry
  } catch {
    /* 取れなければ通常のログインだけ */
  }
})

function done(user) {
  session.user = user
  router.push(session.user.must_change ? '/password' : '/radio')
}

async function submit() {
  error.value = ''
  busy.value = true
  try {
    done(await api.post('/api/login', { id: id.value, password: password.value }))
  } catch (e) {
    error.value = e.message
  } finally {
    busy.value = false
  }
}

async function enterAsGuest() {
  error.value = ''
  busy.value = true
  try {
    done(await api.post('/api/guest', { name: guestName.value }))
  } catch (e) {
    error.value = e.message
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <div class="auth-container">
    <div class="auth-stack">
      <form v-if="openEntry" class="panel auth-panel" @submit.prevent="enterAsGuest">
        <div class="auth-intro"><span class="auth-symbol"><UiIcon name="radio" /></span><h1>パスワードなしで入室</h1><p class="muted">デモ用です。名前を入れて（空欄でも可）入室すると、ID が自動で発行されます</p></div>
        <label for="guest-name">名前（任意）</label>
        <input id="guest-name" v-model="guestName" autocomplete="nickname" maxlength="40" placeholder="例: 山田" />
        <div class="form-actions">
          <button type="submit" :disabled="busy">{{ busy ? '入室中…' : '入室する' }}</button>
        </div>
        <p class="muted auth-note">満員（接続の上限）のときは入室できません。</p>
      </form>
      <form class="panel auth-panel" @submit.prevent="submit">
        <div class="auth-intro"><span v-if="!openEntry" class="auth-symbol"><UiIcon name="radio" /></span><h1>ログイン</h1><p class="muted">IDとパスワードを入力してください</p></div>
        <label for="id">ID</label>
        <input id="id" v-model="id" autocomplete="username" inputmode="numeric" placeholder="例: 0001-0012（イベント ID-番号）" required />
        <label for="pw">パスワード</label>
        <div class="password-field"><input id="pw" v-model="password" :type="showPassword ? 'text' : 'password'" autocomplete="current-password" :required="!openEntry" /><button type="button" :aria-label="showPassword ? 'パスワードを隠す' : 'パスワードを表示'" :aria-pressed="showPassword" @click="showPassword = !showPassword"><UiIcon name="eye" /></button></div>
        <div class="form-actions">
          <button type="submit" :disabled="busy">{{ busy ? 'ログイン中…' : 'ログイン' }}</button>
        </div>
        <p v-if="error" class="error" role="alert">{{ error }}</p>
        <p v-if="openEntry" class="muted auth-note">一般の ID はパスワードを空欄のままログインできます。管理者・スーパーユーザーはパスワードが必要です。</p>
        <p v-else class="muted auth-note">初回ログイン後にパスワードを変更します。</p>
      </form>
    </div>
  </div>
</template>

<style scoped>
.auth-stack {
  display: grid;
  gap: 16px;
}
</style>
