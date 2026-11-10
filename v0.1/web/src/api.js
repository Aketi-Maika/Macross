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
