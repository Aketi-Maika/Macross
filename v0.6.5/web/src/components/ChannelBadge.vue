<script setup>
defineProps({ id: { type: [Number, String], default: null } })
</script>
<template><span class="channel-badge" :aria-label="id == null ? 'チャンネル未選択' : `チャンネル ${id}`"><small>CH</small><strong>{{ id ?? '—' }}</strong></span></template>
