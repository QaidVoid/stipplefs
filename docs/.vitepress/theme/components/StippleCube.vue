<script setup lang="ts">
import { onBeforeUnmount, onMounted, ref } from 'vue'

const props = withDefaults(defineProps<{ n?: number; animate?: boolean }>(), {
  n: 8,
  animate: true,
})

type Dot = { x: number; y: number; r: number; depth: number }

const SIZE = 128
const EDGE = 62
const GUTTER = 0.07
// Radius as a fraction of the dot pitch, at the start and end of each face.
const SHADES: [number, number][] = [
  [0.13, 0.2],
  [0.27, 0.33],
  [0.36, 0.42],
]

function faces(cx: number, cy: number, a: number) {
  const c = Math.cos(Math.PI / 6)
  const p = {
    top: [cx, cy - a],
    right: [cx + a * c, cy - a / 2],
    centre: [cx, cy],
    left: [cx - a * c, cy - a / 2],
    bl: [cx - a * c, cy + a / 2],
    bottom: [cx, cy + a],
  }
  const sub = (u: number[], v: number[]) => [u[0] - v[0], u[1] - v[1]]
  return [
    [p.top, sub(p.right, p.top), sub(p.left, p.top)],
    [p.left, sub(p.centre, p.left), sub(p.bl, p.left)],
    [p.centre, sub(p.right, p.centre), sub(p.bottom, p.centre)],
  ]
}

function build(n: number): Dot[] {
  const pitch = EDGE / n
  const dots: Dot[] = []
  faces(SIZE / 2, SIZE / 2, EDGE).forEach(([o, e1, e2], f) => {
    const [r0, r1] = SHADES[f]
    for (let i = 0; i < n; i++) {
      for (let j = 0; j < n; j++) {
        const s = GUTTER + (1 - 2 * GUTTER) * ((i + 0.5) / n)
        const t = GUTTER + (1 - 2 * GUTTER) * ((j + 0.5) / n)
        const x = o[0] + s * e1[0] + t * e2[0]
        const y = o[1] + s * e1[1] + t * e2[1]
        dots.push({ x, y, r: (r0 + (r1 - r0) * t) * pitch, depth: y / SIZE })
      }
    }
  })
  return dots
}

const dots = build(props.n)
const svg = ref<SVGSVGElement>()
let frame = 0
let visible = true
let pointer: { x: number; y: number } | null = null
let observer: IntersectionObserver | undefined

function move(e: PointerEvent) {
  const box = svg.value!.getBoundingClientRect()
  pointer = {
    x: ((e.clientX - box.left) / box.width) * SIZE,
    y: ((e.clientY - box.top) / box.height) * SIZE,
  }
}

function leave() {
  pointer = null
}

onMounted(() => {
  if (!props.animate || matchMedia('(prefers-reduced-motion: reduce)').matches) return
  const circles = Array.from(svg.value!.querySelectorAll('circle'))
  const start = performance.now()
  observer = new IntersectionObserver(([entry]) => (visible = entry.isIntersecting))
  observer.observe(svg.value!)

  const tick = (now: number) => {
    frame = requestAnimationFrame(tick)
    if (!visible) return
    const t = (now - start) / 1000
    for (let k = 0; k < dots.length; k++) {
      const d = dots[k]
      const wave = 0.5 + 0.5 * Math.sin(t * 1.6 - d.depth * 9)
      let scale = 0.86 + 0.24 * wave * wave
      if (pointer) {
        const dist = Math.hypot(d.x - pointer.x, d.y - pointer.y)
        scale += Math.max(0, 1 - dist / 26) * 0.55
      }
      circles[k].setAttribute('r', (d.r * scale).toFixed(2))
    }
  }
  frame = requestAnimationFrame(tick)
})

onBeforeUnmount(() => {
  cancelAnimationFrame(frame)
  observer?.disconnect()
})
</script>

<template>
  <svg
    ref="svg"
    class="stipple-cube"
    :viewBox="`0 0 ${SIZE} ${SIZE}`"
    role="img"
    aria-label="A block drawn in dots"
    @pointermove="move"
    @pointerleave="leave"
  >
    <circle
      v-for="(d, k) in dots"
      :key="k"
      :cx="d.x.toFixed(2)"
      :cy="d.y.toFixed(2)"
      :r="d.r.toFixed(2)"
      :style="{ animationDelay: `${(d.depth * 0.9 + (k % 7) * 0.03).toFixed(2)}s` }"
    />
  </svg>
</template>

<style scoped>
.stipple-cube {
  width: 100%;
  height: auto;
  overflow: visible;
  fill: var(--st-vermilion);
  touch-action: none;
}

circle {
  transform-box: fill-box;
  transform-origin: center;
  animation: settle 0.7s cubic-bezier(0.2, 0.9, 0.3, 1.2) both;
}

@keyframes settle {
  from {
    opacity: 0;
    transform: scale(0);
  }
}
</style>
