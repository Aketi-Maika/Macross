<script setup>
defineProps({ name: { type: String, required: true } })
const paths = {
  radio: ['M9 3h6v18H5V7h10', 'M10 3V1', 'M8 11h4M8 15h4M8 18h4', 'M18 5a5 5 0 0 1 0 8M20 2a9 9 0 0 1 0 14'],
  settings: ['M9 3h6l1 3 3 1 2 5-2 5-3 1-1 3H9l-1-3-3-1-2-5 2-5 3-1z', 'M16 12a4 4 0 1 1-8 0 4 4 0 0 1 8 0'],
  map: ['M20 10c0 6-8 12-8 12S4 16 4 10a8 8 0 1 1 16 0', 'M15 10a3 3 0 1 1-6 0 3 3 0 0 1 6 0'],
  mic: ['M8 5a4 4 0 0 1 8 0v7a4 4 0 0 1-8 0z', 'M5 10v2a7 7 0 0 0 14 0v-2M12 19v4M8 23h8'],
  monitor: ['M3 3h18v13H3z', 'M12 16v5M7 21h10'],
  user: ['M16 6a4 4 0 1 1-8 0 4 4 0 0 1 8 0', 'M3 21v-2a9 9 0 0 1 18 0v2z'],
  lock: ['M6 10h12v12H6z', 'M8 10V6a4 4 0 1 1 8 0v4M12 15v3'],
  eye: ['M2 12s4-7 10-7 10 7 10 7-4 7-10 7S2 12 2 12', 'M15 12a3 3 0 1 1-6 0 3 3 0 0 1 6 0'],
  mail: ['M2 4h20v16H2z', 'm2 5 10 8L22 5'],
  server: ['M3 3h18v7H3zM3 14h18v7H3z', 'M6 6.5h1M6 17.5h1M11 6.5h7M11 17.5h7'],
  headphones: ['M3 14V11a9 9 0 0 1 18 0v3', 'M3 12h4v9H3zM17 12h4v9h-4z'],
  volume: ['M3 9h4l5-5v16l-5-5H3z', 'M16 8a6 6 0 0 1 0 8M19 5a10 10 0 0 1 0 14'],
  info: ['M22 12a10 10 0 1 1-20 0 10 10 0 0 1 20 0', 'M12 11v6M12 7h.01'],
  check: ['m5 12 4 4L19 6'], chevron: ['m8 4 8 8-8 8'],
  download: ['M12 2v13m-5-5 5 5 5-5', 'M3 16v5h18v-5'],
  folder: ['M2 5h8l2 3h10v13H2z'], file: ['M5 2h9l5 5v15H5zM14 2v6h5'],
  refresh: ['M20 7a9 9 0 1 0 1 8', 'M20 2v6h-6'], logout: ['M9 3H3v18h6M9 12h12m-5-5 5 5-5 5'],
  shield: ['m12 2 9 4v6c0 6-9 10-9 10S3 18 3 12V6z', 'm8 12 3 3 5-6'],
  calendar: ['M3 5h18v16H3z', 'M3 10h18M8 2v5M16 2v5'],
}
</script>
<template><svg class="ui-icon" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="1.8" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true" focusable="false"><path v-for="(path, index) in paths[name] || paths.info" :key="index" :d="path" /></svg></template>
