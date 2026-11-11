<script setup>
import { useRouter } from 'vue-router'
import { session, logout } from './api.js'

const router = useRouter()

async function onLogout() {
  await logout()
  router.push('/login')
}
</script>

<template>
  <header class="header">
    <div class="brand">Macross <span class="muted">v0.2</span></div>
    <nav v-if="session.user && !session.user.must_change" class="row">
      <router-link to="/radio">無線</router-link>
      <router-link v-if="session.user.admin" to="/admin">管理</router-link>
      <a v-if="session.user.admin" href="/files/" target="_blank" rel="noopener">録音</a>
      <a v-if="session.user.admin" href="/grafana/" target="_blank" rel="noopener">監視</a>
      <span class="muted">{{ session.user.name }} ({{ session.user.id }})</span>
      <button class="secondary small" @click="onLogout">ログアウト</button>
    </nav>
  </header>
  <router-view />
</template>

<style scoped>
.header {
  display: flex;
  justify-content: space-between;
  align-items: center;
  gap: 12px;
  padding: 10px 16px;
  border-bottom: 1px solid var(--line);
  flex-wrap: wrap;
}
.brand { font-weight: 700; letter-spacing: .05em; }
nav a { text-decoration: none; }
nav a.router-link-active { font-weight: 600; }
.small { padding: 6px 10px; font-size: .8rem; }
</style>
