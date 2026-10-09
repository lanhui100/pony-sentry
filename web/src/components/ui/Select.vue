<template>
  <div class="relative">
    <select
      :value="modelValue"
      @change="$emit('update:modelValue', $event.target.value)"
      :class="cn(
        'h-8 appearance-none rounded-md border border-stone-800 bg-stone-900/60 pl-2.5 pr-7 py-1 text-xs text-stone-200 transition-colors focus-visible:outline-none focus-visible:border-stone-600 focus-visible:ring-1 focus-visible:ring-stone-600 cursor-pointer font-medium',
        $attrs.class
      )"
      v-bind="$attrs"
    >
      <slot />
    </select>
    <div class="pointer-events-none absolute inset-y-0 right-0 flex items-center pr-2 text-stone-500">
      <svg class="h-3.5 w-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2">
        <path d="m6 9 6 6 6-6"/>
      </svg>
    </div>
  </div>
</template>

<script setup>
import { cn } from '@/lib/utils'

defineProps({
  modelValue: {
    type: [String, Number],
    default: ''
  }
})

defineEmits(['update:modelValue'])
</script>
