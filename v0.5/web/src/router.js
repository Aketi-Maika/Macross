import { createRouter, createWebHashHistory } from 'vue-router'
import { session, loadSession } from './api.js'
import Login from './views/Login.vue'
import Password from './views/Password.vue'
import Radio from './views/Radio.vue'
import Admin from './views/Admin.vue'

// 地図（MapLibre）は大きいので、開いたときに読み込む
const MapView = () => import('./views/Map.vue')
const Recordings = () => import('./views/Recordings.vue')

const router = createRouter({
  history: createWebHashHistory(),
  routes: [
    { path: '/', redirect: '/radio' },
    { path: '/login', component: Login, meta: { public: true } },
    { path: '/password', component: Password },
    { path: '/radio', component: Radio },
    { path: '/admin', component: Admin, meta: { admin: true } },
    { path: '/map', component: MapView, meta: { admin: true, wide: true } },
    { path: '/recordings', component: Recordings, meta: { admin: true, wide: true } },
  ],
})

router.beforeEach(async (to) => {
  if (session.user === undefined) await loadSession()
  const u = session.user
  if (to.meta.public) return u ? (u.must_change ? '/password' : '/radio') : true
  if (!u) return '/login'
  if (u.must_change && to.path !== '/password') return '/password'
  if (to.meta.admin && !u.admin) return '/radio'
  return true
})

export default router
