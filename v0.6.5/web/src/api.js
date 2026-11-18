import { reactive } from 'vue'

/** ログイン中ユーザ（null = 未ログイン, undefined = 未確認） */
export const session = reactive({ user: undefined })

async function request(method, path, body) {
  const res = await fetch(path, {
    method,
    credentials: 'same-origin',
    headers: body ? { 'Content-Type': 'application/json' } : {},
    body: body ? JSON.stringify(body) : undefined,
  })
  if (res.status === 204) return null
  const data = await res.json().catch(() => ({}))
  if (!res.ok) {
    // 管理者の権限が外された・ログインが切れた: 画面のログイン情報を取り直す（メニューを最新に）
    if ((res.status === 403 && path.startsWith('/api/admin')) || (res.status === 401 && path !== '/api/me' && path !== '/api/login')) {
      onAuthChanged()
    }
    const err = new Error(data.error || `エラーが発生しました (${res.status})`)
    err.status = res.status
    throw err
  }
  return data
}

export const api = {
  get: (p) => request('GET', p),
  post: (p, b) => request('POST', p, b),
  put: (p, b) => request('PUT', p, b),
  patch: (p, b) => request('PATCH', p, b),
  del: (p) => request('DELETE', p),
}

let authCheck = null
function onAuthChanged() {
  if (authCheck) return
  authCheck = loadSession().finally(() => {
    authCheck = null
    const hash = window.location.hash
    if (!session.user) window.location.hash = '#/login'
    else if (!session.user.admin && /^#\/(admin|map|recordings)/.test(hash)) window.location.hash = '#/radio'
  })
}

export async function loadSession() {
  try {
    session.user = await api.get('/api/me')
  } catch {
    session.user = null
  }
  return session.user
}

export async function logout() {
  await api.post('/api/logout').catch(() => {})
  session.user = null
}
