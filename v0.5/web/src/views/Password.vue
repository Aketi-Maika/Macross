<script setup>
import { ref } from 'vue'
import { useRouter } from 'vue-router'
import { api, session } from '../api.js'
import UiIcon from '../components/UiIcon.vue'

const router = useRouter()
const current = ref('')
const next = ref('')
const confirm = ref('')
const error = ref('')
const busy = ref(false)
const show = ref({ current: false, next: false, confirm: false })

async function submit() {
  error.value = ''
  if (next.value !== confirm.value) {
    error.value = '新しいパスワードが一致しません'
    return
  }
  busy.value = true
  try {
    session.user = await api.post('/api/password', { current: current.value, new: next.value })
    router.push('/radio')
  } catch (e) {
    error.value = e.message
  } finally {
    busy.value = false
  }
}
</script>

<template>
  <div class="auth-container password-container">
    <form class="panel auth-panel" @submit.prevent="submit">
      <h2 class="password-heading"><UiIcon name="lock" />新しいパスワードは8文字以上で設定してください</h2>
      <p v-if="session.user?.must_change" class="muted">
        初回ログインです。初期パスワードを自分のパスワードに変更してください。
      </p>
      <label for="cur">現在のパスワード</label>
      <div class="password-field"><input id="cur" v-model="current" :type="show.current ? 'text' : 'password'" autocomplete="current-password" required /><button type="button" :aria-label="show.current ? '現在のパスワードを隠す' : '現在のパスワードを表示'" :aria-pressed="show.current" @click="show.current = !show.current"><UiIcon name="eye" /></button></div>
      <label for="new">新しいパスワード（8文字以上）</label>
      <div class="password-field"><input id="new" v-model="next" :type="show.next ? 'text' : 'password'" autocomplete="new-password" minlength="8" required /><button type="button" :aria-label="show.next ? '新しいパスワードを隠す' : '新しいパスワードを表示'" :aria-pressed="show.next" @click="show.next = !show.next"><UiIcon name="eye" /></button></div>
      <label for="conf">新しいパスワード（確認）</label>
      <div class="password-field"><input id="conf" v-model="confirm" :type="show.confirm ? 'text' : 'password'" autocomplete="new-password" minlength="8" required /><button type="button" :aria-label="show.confirm ? '確認用パスワードを隠す' : '確認用パスワードを表示'" :aria-pressed="show.confirm" @click="show.confirm = !show.confirm"><UiIcon name="eye" /></button></div>
      <div class="form-actions">
        <button type="submit" :disabled="busy">{{ busy ? '変更中…' : '変更する' }}</button>
      </div>
      <p v-if="error" class="error" role="alert">{{ error }}</p>
      <p class="muted auth-note"><UiIcon name="info" /> 初回ログイン時も、ここで初期パスワードを変更します。</p>
    </form>
  </div>
</template>

<style scoped>
.password-container { max-width: 880px; margin-top: 20px; }
.password-heading { display: flex; align-items: center; gap: 12px; font-size: 1.1rem; }
</style>
