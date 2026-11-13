<script setup>
import { theme } from '../ui/theme.js'
const themes = [{ id: 'dark', label: 'ダーク' }, { id: 'white', label: 'ホワイト' }, { id: 'gray', label: 'グレー' }]
</script>
<template>
  <div class="theme-control" role="group" aria-label="表示テーマ">
    <span class="theme-label">テーマ</span>
    <div class="segmented"><button v-for="item in themes" :key="item.id" type="button" :aria-pressed="theme === item.id" :class="{ selected: theme === item.id }" @click="theme = item.id">{{ item.label }}</button></div>
  </div>
</template>
