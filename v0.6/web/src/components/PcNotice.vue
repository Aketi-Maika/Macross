<script setup>
/** スマホ・タブレットで管理系の画面を開いたときの「PC 推奨」のお知らせ（閉じたら次からは出さない） */
import { ref, onMounted, onBeforeUnmount } from 'vue'
import UiIcon from './UiIcon.vue'

const KEY = 'macross.pcNoticeClosed'
const small = ref(false)
const closed = ref(false)
let mq = null

function update() {
  small.value = !!mq?.matches
}

try {
  closed.value = localStorage.getItem(KEY) === '1'
} catch {
  /* 保存できない環境では毎回出す */
}

function close() {
  closed.value = true
  try {
    localStorage.setItem(KEY, '1')
  } catch {
    /* 保存できなくても閉じる */
  }
}

onMounted(() => {
  mq = window.matchMedia('(max-width: 760px), (pointer: coarse)')
  update()
  mq.addEventListener?.('change', update)
})
onBeforeUnmount(() => mq?.removeEventListener?.('change', update))
</script>

<template>
  <div v-if="small && !closed" class="pc-notice" role="note">
    <UiIcon name="monitor" />
    <span>管理画面・地図・録音は <strong>PC での操作を推奨</strong>します。スマホでは表示が詰まって見える場合があります。</span>
    <button type="button" class="secondary" aria-label="お知らせを閉じる" @click="close">閉じる</button>
  </div>
</template>

<style scoped>
.pc-notice { display: flex; align-items: center; gap: 10px; margin: 12px 0; padding: 10px 12px; border: 1px solid var(--line); border-left: 4px solid var(--warn); border-radius: 8px; background: var(--panel); font-size: .88rem; }
.pc-notice span { flex: 1; }
.pc-notice button { min-height: 36px; padding: 6px 12px; font-size: .82rem; }
</style>
