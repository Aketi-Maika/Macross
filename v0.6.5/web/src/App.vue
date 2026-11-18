<script setup>
import { computed } from 'vue'
import { useRoute, useRouter } from 'vue-router'
import { session, logout } from './api.js'
import ThemeSwitch from './components/ThemeSwitch.vue'
import UiIcon from './components/UiIcon.vue'
import PcNotice from './components/PcNotice.vue'
import { theme } from './ui/theme.js'

const router = useRouter()
const route = useRoute()
const titles = { '/radio': '無線', '/admin': '管理', '/map': '地図', '/password': 'パスワード変更', '/recordings': '録音' }
const title = computed(() => titles[route.path] || '')
const activeSession = computed(() => session.user && !session.user.must_change)
const grafanaUrl = computed(() => `/grafana/?theme=${theme.value === 'dark' ? 'dark' : 'light'}`)

function focusMain() {
  document.getElementById('main-content')?.focus()
}

async function onLogout() {
  await logout()
  router.push('/login')
}
</script>

<template>
  <a class="skip-link" href="#main-content" @click.prevent="focusMain">本文へ移動</a>
  <header class="app-header">
    <router-link class="brand" :to="activeSession ? '/radio' : session.user ? '/password' : '/login'" aria-label="Macross ホーム">
      <img :src="theme === 'dark' ? '/macross-logo-dark.png' : '/macross-logo.png'" alt="Macross — Build Maika" width="1600" height="344" /><span class="version">v0.6</span>
    </router-link>
    <nav v-if="activeSession" class="main-nav" aria-label="メインメニュー">
      <router-link to="/radio"><UiIcon name="radio" />無線</router-link>
      <router-link v-if="session.user.admin" to="/admin"><UiIcon name="settings" />管理</router-link>
      <router-link v-if="session.user.admin" to="/map"><UiIcon name="map" />地図</router-link>
      <router-link v-if="session.user.admin" to="/recordings"><UiIcon name="mic" />録音</router-link>
      <a v-if="session.user.admin" class="nav-pc" :href="grafanaUrl" target="_blank" rel="noopener" title="Grafanaを新しいタブで開く"><UiIcon name="monitor" />監視<span class="external-mark" aria-label="新しいタブ">↗</span></a>
    </nav>
    <details v-if="session.user" class="account-menu">
      <summary><UiIcon name="user" /><span>{{ session.user.name }} <span class="account-id">({{ session.user.display_id ?? session.user.id }})</span></span><span aria-hidden="true">⌄</span></summary>
      <div class="account-popover">
        <span class="account-role">{{ session.user.superuser ? 'スーパーユーザー' : session.user.admin ? '管理者' : '一般ユーザー' }}</span>
        <router-link to="/password" @click="$event.currentTarget.closest('details').removeAttribute('open')"><UiIcon name="lock" />パスワード変更</router-link>
        <ThemeSwitch class="mobile-only" />
        <button type="button" class="secondary" @click="onLogout"><UiIcon name="logout" />ログアウト</button>
      </div>
    </details>
    <ThemeSwitch v-else />
  </header>
  <main id="main-content" tabindex="-1">
    <div v-if="title" class="page-heading">
      <div class="heading-title"><h1>{{ title }}</h1><span v-if="session.user?.admin && route.path !== '/password'" class="role-badge" :title="session.user.superuser ? 'スーパーユーザー' : '管理者'"><UiIcon name="shield" /><span class="role-text">{{ session.user.superuser ? 'スーパーユーザー' : '管理者' }}</span></span></div>
      <ThemeSwitch />
    </div>
    <div v-if="route.meta.admin" class="container pc-notice-wrap"><PcNotice /></div>
    <!-- 無線画面は他の画面へ移っても接続したまま残す（ログアウトすると破棄して切断） -->
    <router-view v-slot="{ Component }">
      <keep-alive :include="activeSession ? ['Radio'] : []"><component :is="Component" /></keep-alive>
    </router-view>
  </main>
</template>

