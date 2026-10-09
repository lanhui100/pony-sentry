<template>
  <div
    :class="cn('rounded-md border border-stone-800 bg-stone-900/40 animate-pulse', $attrs.class)"
  />
</template>

<script setup>
import { cn } from '@/lib/utils'
</script>
