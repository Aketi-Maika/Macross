// Existing File Browser v2 API, behind the existing Macross admin gate.
// This UI only lists and reads recordings; it never modifies stored files.
let token = ''
let pendingLogin = null
async function login() {
  if (!pendingLogin) pendingLogin = (async () => {
    const response = await fetch('/files/api/login', { method: 'POST', credentials: 'same-origin' })
    if (!response.ok) throw new Error('録音サービスに接続できません。管理者としてログインしているか確認してください。')
    const value = (await response.text()).trim()
    if (!/^[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+\.[A-Za-z0-9_-]+$/.test(value)) throw new Error('録音サービスから正しい応答がありません。')
    token = value
  })().finally(() => { pendingLogin = null })
  await pendingLogin
}
function encodedPath(path) {
  return '/' + path.split('/').filter(Boolean).map(encodeURIComponent).join('/')
}
async function read(kind, path, signal, retry = true) {
  if (!token) await login()
  const response = await fetch(`/files/api/${kind}${encodedPath(path)}`, { credentials: 'same-origin', headers: { 'X-Auth': token }, signal })
  if (response.status === 401 && retry) {
    token = ''
    await login()
    return read(kind, path, signal, false)
  }
  if (!response.ok) throw new Error(response.status === 403 ? '録音を表示する権限がありません。' : response.status === 404 ? '録音フォルダーまたはファイルが見つかりません。' : `録音を読み込めませんでした（${response.status}）`)
  return response
}
export async function listRecordings(path, signal) {
  const data = await (await read('resources', path, signal)).json()
  if (!data.isDir || !Array.isArray(data.items)) throw new Error('録音フォルダーを開けませんでした。')
  return data.items
}
export async function readRecording(path, signal) {
  return (await read('raw', path, signal)).blob()
}
