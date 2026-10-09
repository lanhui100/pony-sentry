<template>
  <component
    :is="as"
    :class="cn(buttonVariants({ variant, size }), $attrs.class)"
    v-bind="$attrs"
  >
    <slot />
  </component>
</template>

<script setup>
import { cva } from 'class-variance-authority'
import { cn } from '@/lib/utils'

const buttonVariants = cva(
  'inline-flex items-center justify-center gap-2 whitespace-nowrap rounded-md text-xs font-medium transition-all outline-none focus-visible:ring-1 focus-visible:ring-stone-400 disabled:pointer-events-none disabled:opacity-50 select-none cursor-pointer',
  {
    variants: {
      variant: {
        default: 'bg-stone-900 text-stone-50 hover:bg-stone-800 shadow-sm border border-stone-800 active:scale-[0.98]',
        secondary: 'bg-stone-800/80 text-stone-200 hover:bg-stone-800 border border-stone-700/60 active:scale-[0.98]',
        outline: 'border border-stone-800 bg-stone-950/60 hover:bg-stone-900 text-stone-300 hover:text-stone-100',
        ghost: 'text-stone-400 hover:text-stone-100 hover:bg-stone-900/80',
        destructive: 'bg-red-950/60 text-red-300 border border-red-900/60 hover:bg-red-900/40',
        accent: 'bg-stone-100 text-stone-900 hover:bg-stone-200 font-semibold shadow-sm'
      },
      size: {
        default: 'h-8 px-3 py-1.5',
        sm: 'h-7 px-2.5 text-[11px]',
        lg: 'h-9 px-4 text-sm',
        icon: 'h-7 w-7 p-0'
      }
    },
    defaultVariants: {
      variant: 'default',
      size: 'default'
    }
  }
)

defineProps({
  variant: {
    type: String,
    default: 'default'
  },
  size: {
    type: String,
    default: 'default'
  },
  as: {
    type: String,
    default: 'button'
  }
})
</script>
