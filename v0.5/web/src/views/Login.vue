<script setup>
import { ref } from 'vue'
import { useRouter } from 'vue-router'
import { api, session } from '../api.js'
import UiIcon from '../components/UiIcon.vue'

const router = useRouter()
const id = ref('')
const password = ref('')
const error = ref('')
const busy = ref(false)
const showPassword = ref(false)

async function submit() {
  error.value = ''
  busy.value = true
  try {
    session.user = await api.post('/api/login', { id: id.value, password: password.value })
    router.push(session.user.must_change ? '/password' : '/radio')
  } catch (e) {
    error.value = e.message
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <div class="auth-container">
    <form class="panel auth-panel" @submit.prevent="submit">
      <div class="auth-intro"><span class="auth-symbol"><UiIcon name="radio" /></span><h1>ログイン</h1><p class="muted">IDとパスワードを入力してください</p></div>
      <label for="id">ID</label>
      <input id="id" v-model="id" autocomplete="username" inputmode="numeric" required />
      <label for="pw">パスワード</label>
      <div class="password-field"><input id="pw" v-model="password" :type="showPassword ? 'text' : 'password'" autocomplete="current-password" required /><button type="button" :aria-label="showPassword ? 'パスワードを隠す' : 'パスワードを表示'" :aria-pressed="showPassword" @click="showPassword = !showPassword"><UiIcon name="eye" /></button></div>
      <div class="form-actions">
        <button type="submit" :disabled="busy">{{ busy ? 'ログイン中…' : 'ログイン' }}</button>
      </div>
      <p v-if="error" class="error" role="alert">{{ error }}</p>
      <p class="muted auth-note">初回ログイン後にパスワードを変更します。</p>
    </form>
  </div>
</template>

