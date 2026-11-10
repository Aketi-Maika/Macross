<script setup>
import { ref } from 'vue'
import { useRouter } from 'vue-router'
import { api, session } from '../api.js'

const router = useRouter()
const id = ref('')
const password = ref('')
const error = ref('')
const busy = ref(false)

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
  <div class="container narrow">
    <form class="panel" @submit.prevent="submit">
      <h1>ログイン</h1>
      <label for="id">ID</label>
      <input id="id" v-model="id" autocomplete="username" inputmode="numeric" required />
      <label for="pw">パスワード</label>
      <input id="pw" v-model="password" type="password" autocomplete="current-password" required />
      <div style="margin-top: 16px">
        <button type="submit" :disabled="busy">ログイン</button>
      </div>
      <p v-if="error" class="error">{{ error }}</p>
    </form>
  </div>
</template>

<style scoped>
.narrow { max-width: 400px; margin-top: 8vh; }
</style>
