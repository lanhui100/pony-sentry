<template>
  <div :class="cn(badgeVariants({ variant }), $attrs.class)" v-bind="$attrs">
    <slot />
  </div>
</template>

<script setup>
import { cva } from 'class-variance-authority'
import { cn } from '@/lib/utils'

const badgeVariants = cva(
  'inline-flex items-center gap-1.5 rounded-sm px-2 py-0.5 text-[11px] font-mono font-medium tracking-tight border transition-colors',
  {
    variants: {
      variant: {
        default: 'border-stone-800 bg-stone-900/90 text-stone-300',
        secondary: 'border-stone-800 bg-stone-950 text-stone-400',
        outline: 'border-stone-700/60 text-stone-300',
        success: 'border-emerald-900/50 bg-emerald-950/40 text-emerald-400',
        warning: 'border-amber-900/50 bg-amber-950/40 text-amber-400',
        danger: 'border-rose-900/50 bg-rose-950/40 text-rose-400',
        info: 'border-sky-900/50 bg-sky-950/40 text-sky-400',
        neutral: 'border-stone-800 bg-stone-900/40 text-stone-400'
      }
    },
    defaultVariants: {
      variant: 'default'
    }
  }
)

defineProps({
  variant: {
    type: String,
    default: 'default'
  }
})
</script>
