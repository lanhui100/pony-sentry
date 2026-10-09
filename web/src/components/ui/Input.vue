<template>
  <div class="relative w-full">
    <slot name="prefix" />
    <input
      :value="modelValue"
      @input="$emit('update:modelValue', $event.target.value)"
      :type="type"
      :placeholder="placeholder"
      :class="cn(
        'flex h-8 w-full rounded-md border border-stone-800 bg-stone-900/60 px-3 py-1.5 text-xs text-stone-100 placeholder:text-stone-500 transition-colors focus-visible:outline-none focus-visible:border-stone-600 focus-visible:ring-1 focus-visible:ring-stone-600 disabled:cursor-not-allowed disabled:opacity-50',
        $attrs.class
      )"
      v-bind="$attrs"
    />
    <slot name="suffix" />
  </div>
</template>

<script setup>
import { cn } from '@/lib/utils'

defineProps({
  modelValue: {
    type: [String, Number],
    default: ''
  },
  type: {
    type: String,
    default: 'text'
  },
  placeholder: {
    type: String,
    default: ''
  }
})

defineEmits(['update:modelValue'])
</script>
