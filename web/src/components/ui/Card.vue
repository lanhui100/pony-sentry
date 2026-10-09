<template>
  <div :class="cn('rounded-lg border border-stone-800/80 bg-stone-900/40 text-stone-200 shadow-sm backdrop-blur-sm', $attrs.class)">
    <slot />
  </div>
</template>

<script setup>
import { cn } from '@/lib/utils'
</script>
