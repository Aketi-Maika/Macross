<script setup>
import { ref } from 'vue'
import { useRouter } from 'vue-router'
import { api, session } from '../api.js'

const router = useRouter()
const current = ref('')
const next = ref('')
const confirm = ref('')
const error = ref('')
const busy = ref(false)

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
  <div class="container narrow">
    <form class="panel" @submit.prevent="submit">
      <h1>パスワード変更</h1>
      <p v-if="session.user?.must_change" class="muted">
        初回ログインです。初期パスワードを自分のパスワードに変更してください。
      </p>
      <label for="cur">現在のパスワード</label>
      <input id="cur" v-model="current" type="password" autocomplete="current-password" required />
      <label for="new">新しいパスワード（8文字以上）</label>
      <input id="new" v-model="next" type="password" autocomplete="new-password" minlength="8" required />
      <label for="conf">新しいパスワード（確認）</label>
      <input id="conf" v-model="confirm" type="password" autocomplete="new-password" minlength="8" required />
      <div style="margin-top: 16px">
        <button type="submit" :disabled="busy">変更する</button>
      </div>
      <p v-if="error" class="error">{{ error }}</p>
    </form>
  </div>
</template>

<style scoped>
.narrow { max-width: 400px; margin-top: 6vh; }
</style>
