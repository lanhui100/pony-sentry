<template>
  <div :class="cn('inline-flex items-center rounded-lg bg-stone-950/80 p-0.5 border border-stone-800/80 text-stone-400', $attrs.class)">
    <button
      v-for="item in items"
      :key="item.value"
      type="button"
      @click="$emit('update:modelValue', item.value)"
      :class="cn(
        'relative inline-flex items-center gap-1.5 rounded-md px-3 py-1.5 text-xs font-medium transition-all select-none cursor-pointer',
        modelValue === item.value
          ? 'bg-stone-800 text-stone-100 shadow-sm border border-stone-700/60'
          : 'hover:text-stone-200 hover:bg-stone-900/50 text-stone-400'
      )"
    >
      <component v-if="item.icon" :is="item.icon" class="h-3.5 w-3.5" />
      <span>{{ item.label }}</span>
    </button>
  </div>
</template>

<script setup>
import { cn } from '@/lib/utils'

defineProps({
  modelValue: {
    type: String,
    required: true
  },
  items: {
    type: Array,
    required: true
  }
})

defineEmits(['update:modelValue'])
</script>
