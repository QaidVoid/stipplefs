<script setup lang="ts">
import { withBase } from 'vitepress'
import StippleCube from './StippleCube.vue'

const stats = [
  { value: '1.4 GiB → 453 MiB', label: 'packed into one image with 32 MiB blocks' },
  { value: '0.73 s', label: 'first launch, mount included, against 0.50 s from a plain folder' },
  { value: '0.50 s', label: 'every launch after that, the same as from a plain folder' },
  { value: '437 MiB', label: 'held by the mount daemon while the application starts' },
]

const features = [
  {
    title: 'Start-up comes first',
    body: 'Files an application opens while starting go to the front of the image. Background workers decode them and hand the bytes to the kernel page cache before anything asks.',
    shade: 3,
  },
  {
    title: 'Deduplication inside files',
    body: 'Content-defined chunking finds shared runs of bytes between files that differ, so near-copies cost almost nothing.',
    shade: 2,
  },
  {
    title: 'Executables shrink further',
    body: 'Every block is also tried through an x86 branch filter, and the smaller encoding wins. Native code gives the compressor far more to work with.',
    shade: 1,
  },
  {
    title: 'Checksummed throughout',
    body: 'The superblock, every metadata section and every block carry an XXH3 digest. Nothing is returned before it is verified.',
    shade: 1,
  },
  {
    title: 'Lives inside other files',
    body: 'Every offset is relative to the image, so an image can sit at any position inside a launcher, an executable or an archive.',
    shade: 2,
  },
  {
    title: 'Specified, not just implemented',
    body: 'A normative spec, checked by an independent reader in the test suite, with a canonical encoding that makes builds reproducible.',
    shade: 3,
  },
]

const steps = [
  { title: 'Mount', body: 'The superblock and metadata are verified, then the mount answers lookups at once.' },
  { title: 'Warm', body: 'Workers decode the hot region, one block each, in parallel.' },
  { title: 'Store', body: "Decoded file bytes go straight into the kernel's page cache." },
  { title: 'Run', body: 'The application reads from memory. Blocks it never asks for are never decoded.' },
]
</script>

<template>
  <div class="st-home">
    <section class="hero">
      <div class="hero-text">
        <p class="eyebrow"><span class="pip" />read-only · compressed · deduplicating</p>
        <h1>
          Pack a whole tree into one file.
          <span class="accent">Start from it fast.</span>
        </h1>
        <p class="lead">
          stipplefs is a filesystem image format and FUSE mount for Linux. It packs a directory into a single
          compressed, deduplicated, checksummed file, then mounts it so an application starts almost as fast as it
          would from disk.
        </p>
        <div class="actions">
          <a class="btn brand" :href="withBase('/guide/quick-start')">Get started</a>
          <a class="btn alt" :href="withBase('/guide/introduction')">Why stipplefs</a>
          <a class="btn ghost" :href="withBase('/format')">Read the spec</a>
        </div>
      </div>
      <div class="hero-art">
        <StippleCube :n="9" />
      </div>
    </section>

    <section class="stats" aria-label="Measured on Blender 5.2">
      <div v-for="s in stats" :key="s.value" class="stat">
        <div class="stat-value">{{ s.value }}</div>
        <div class="stat-label">{{ s.label }}</div>
      </div>
      <p class="stats-note">
        Blender 5.2, a 1.4 GiB application tree, packed with the defaults and a hot list, then launched in the
        background on a 16-thread machine. The build took 86 seconds.
        <a :href="withBase('/guide/tuning')">How the defaults were chosen</a>
      </p>
    </section>

    <section class="features">
      <h2>What goes into an image</h2>
      <div class="grid">
        <article v-for="f in features" :key="f.title" class="card" :data-shade="f.shade">
          <div class="swatch" aria-hidden="true" />
          <h3>{{ f.title }}</h3>
          <p>{{ f.body }}</p>
        </article>
      </div>
    </section>

    <section class="flow">
      <h2>A cold start, step by step</h2>
      <ol>
        <li v-for="(s, i) in steps" :key="s.title">
          <span class="num">{{ i + 1 }}</span>
          <h3>{{ s.title }}</h3>
          <p>{{ s.body }}</p>
        </li>
      </ol>
    </section>

    <section class="terminal-wrap">
      <div class="terminal-copy">
        <h2>Three commands</h2>
        <p>
          Build an image from a directory, verify every byte of it, and mount it. The same image can be extracted
          back to a directory or read straight from Rust.
        </p>
        <a class="more" :href="withBase('/guide/quick-start')">Walk through the quick start →</a>
      </div>
      <div class="terminal" role="figure" aria-label="Example session">
        <div class="bar"><i /><i /><i /></div>
        <pre><code><span class="p">$</span> mkstipple /usr/lib/python3.14 python.stipple
<span class="o">building python.stipple from /usr/lib/python3.14: block size 33554432, level 19, 16 threads
[    0.7s] placing      3595/3595 files, 69.2 MiB read
[   18.1s] compressing  2/2 blocks, 53.8 -> 12.3 MiB (22.9%), 3.1 MiB/s
wrote python.stipple (12959744 bytes) from /usr/lib/python3.14</span>

<span class="p">$</span> stipple check python.stipple | tail -1
<span class="o">verified        72555234 bytes read and checksummed in 0.084 s (827.8 MB/s)</span>

<span class="p">$</span> stipple mount python.stipple ~/mnt
<span class="o">mounted on /home/you/mnt, interrupt to unmount</span></code></pre>
      </div>
    </section>

    <section class="closing">
      <div class="closing-dots" aria-hidden="true" />
      <h2>Small format. Short spec. Fast start.</h2>
      <p>One Rust crate, two command-line tools, and a specification precise enough to write your own reader.</p>
      <div class="actions center">
        <a class="btn brand" :href="withBase('/guide/quick-start')">Get started</a>
        <a class="btn alt" :href="withBase('/guide/library')">Use the library</a>
      </div>
    </section>
  </div>
</template>

<style scoped>
.st-home {
  max-width: 1180px;
  margin: 0 auto;
  padding: 0 24px 96px;
}

h1,
h2,
h3,
.stat-value,
.btn,
.num {
  font-family: var(--st-font-display);
}

h2 {
  font-size: clamp(28px, 4vw, 40px);
  font-weight: 750;
  letter-spacing: -0.03em;
  line-height: 1.1;
  margin: 0 0 32px;
}

/* -------------------------------------------------------------- hero --- */

.hero {
  display: grid;
  grid-template-columns: 1.15fr 0.85fr;
  align-items: center;
  gap: 48px;
  padding: 72px 0 56px;
}

.eyebrow {
  display: inline-flex;
  align-items: center;
  gap: 10px;
  margin: 0 0 22px;
  padding: 6px 14px 6px 10px;
  border: 1px dotted var(--vp-c-border);
  border-radius: 999px;
  background: var(--vp-c-bg);
  color: var(--vp-c-text-2);
  font-size: 13px;
  font-weight: 600;
  letter-spacing: 0.04em;
}

.pip {
  width: 8px;
  height: 8px;
  border-radius: 50%;
  background: var(--st-vermilion);
  box-shadow: 0 0 0 4px rgba(255, 90, 54, 0.18);
}

h1 {
  margin: 0;
  font-size: clamp(40px, 6.4vw, 76px);
  font-weight: 800;
  line-height: 0.98;
  letter-spacing: -0.045em;
  color: var(--vp-c-text-1);
}

.accent {
  display: block;
  color: var(--st-vermilion);
}

.lead {
  max-width: 600px;
  margin: 28px 0 0;
  font-size: 19px;
  line-height: 1.6;
  color: var(--vp-c-text-2);
}

.actions {
  display: flex;
  flex-wrap: wrap;
  gap: 12px;
  margin-top: 36px;
}

.actions.center {
  justify-content: center;
}

.btn {
  display: inline-flex;
  align-items: center;
  height: 46px;
  padding: 0 22px;
  border-radius: 999px;
  font-size: 15px;
  font-weight: 650;
  text-decoration: none;
  transition: transform 0.2s, background-color 0.2s, border-color 0.2s, box-shadow 0.2s;
}

.btn:hover {
  transform: translateY(-2px);
}

.btn.brand {
  background: var(--st-vermilion);
  color: var(--st-ink);
  box-shadow: 0 8px 22px -10px rgba(255, 90, 54, 0.9);
}

.btn.brand:hover {
  background: #ff7352;
}

.btn.alt {
  background: var(--vp-c-text-1);
  color: var(--vp-c-bg);
}

.btn.ghost {
  border: 1px dashed var(--vp-c-border);
  color: var(--vp-c-text-1);
}

.btn.ghost:hover {
  border-color: var(--st-vermilion);
}

.hero-art {
  position: relative;
  max-width: 460px;
  justify-self: center;
  width: 100%;
}

.hero-art::before {
  content: '';
  position: absolute;
  inset: 8%;
  border-radius: 50%;
  background: radial-gradient(closest-side, rgba(255, 90, 54, 0.16), transparent);
  filter: blur(20px);
  z-index: -1;
}

/* ------------------------------------------------------------- stats --- */

.stats {
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  border-top: 2px solid var(--vp-c-text-1);
  border-bottom: 2px dotted var(--vp-c-divider);
  margin-bottom: 96px;
}

.stat {
  padding: 26px 22px 24px 0;
}

.stat + .stat {
  padding-left: 22px;
  border-left: 1px dotted var(--vp-c-border);
}

.stat-value {
  font-size: clamp(24px, 3vw, 34px);
  font-weight: 750;
  letter-spacing: -0.03em;
  white-space: nowrap;
}

.stat-label {
  margin-top: 6px;
  font-size: 14px;
  line-height: 1.5;
  color: var(--vp-c-text-2);
}

.stats-note {
  grid-column: 1 / -1;
  margin: 0;
  padding: 14px 0 18px;
  border-top: 1px dotted var(--vp-c-border);
  font-size: 13.5px;
  color: var(--vp-c-text-3);
}

.stats-note a {
  color: var(--vp-c-brand-1);
  font-weight: 600;
  margin-left: 4px;
}

/* ---------------------------------------------------------- features --- */

.features {
  margin-bottom: 96px;
}

.grid {
  display: grid;
  grid-template-columns: repeat(3, 1fr);
  gap: 18px;
}

.card {
  position: relative;
  padding: 26px 24px 24px;
  border: 1px dotted var(--vp-c-border);
  border-radius: 16px;
  background: var(--vp-c-bg);
  transition: transform 0.25s, border-color 0.25s, box-shadow 0.25s;
}

.card:hover {
  transform: translateY(-3px);
  border-color: var(--st-vermilion);
  box-shadow: 0 18px 40px -28px rgba(0, 0, 0, 0.5);
}

.swatch {
  width: 44px;
  height: 44px;
  margin-bottom: 18px;
  border-radius: 10px;
  background-image: radial-gradient(var(--st-vermilion) var(--r), transparent calc(var(--r) + 0.6px));
  background-size: 8px 8px;
}

.card[data-shade='1'] .swatch {
  --r: 1.3px;
}
.card[data-shade='2'] .swatch {
  --r: 2.2px;
}
.card[data-shade='3'] .swatch {
  --r: 3.1px;
}

.card h3 {
  margin: 0 0 8px;
  font-size: 20px;
  font-weight: 700;
  letter-spacing: -0.02em;
}

.card p {
  margin: 0;
  font-size: 15px;
  line-height: 1.65;
  color: var(--vp-c-text-2);
}

/* -------------------------------------------------------------- flow --- */

.flow {
  margin-bottom: 96px;
}

.flow ol {
  position: relative;
  display: grid;
  grid-template-columns: repeat(4, 1fr);
  gap: 28px;
  margin: 0;
  padding: 0;
  list-style: none;
}

.flow ol::before {
  content: '';
  position: absolute;
  top: 21px;
  left: 22px;
  right: 22px;
  border-top: 3px dotted var(--st-vermilion);
  opacity: 0.6;
}

.flow li {
  position: relative;
}

.num {
  display: grid;
  place-items: center;
  width: 44px;
  height: 44px;
  margin-bottom: 18px;
  border-radius: 50%;
  background: var(--vp-c-text-1);
  color: var(--vp-c-bg);
  font-weight: 750;
  font-size: 18px;
  box-shadow: 0 0 0 6px var(--vp-c-bg);
}

.flow h3 {
  margin: 0 0 6px;
  font-size: 20px;
  font-weight: 700;
}

.flow p {
  margin: 0;
  font-size: 15px;
  line-height: 1.65;
  color: var(--vp-c-text-2);
}

/* ---------------------------------------------------------- terminal --- */

.terminal-wrap {
  display: grid;
  grid-template-columns: 0.8fr 1.2fr;
  gap: 48px;
  align-items: center;
  margin-bottom: 110px;
}

.terminal-copy p {
  margin: 0 0 18px;
  font-size: 17px;
  line-height: 1.7;
  color: var(--vp-c-text-2);
}

.more {
  color: var(--vp-c-brand-1);
  font-weight: 650;
  text-decoration: none;
}

.more:hover {
  text-decoration: underline;
}

.terminal {
  border-radius: 16px;
  background: var(--st-code-bg);
  box-shadow: 0 30px 60px -30px rgba(0, 0, 0, 0.6);
  overflow: hidden;
}

.bar {
  display: flex;
  gap: 7px;
  padding: 14px 16px;
  border-bottom: 1px dotted #3a352e;
}

.bar i {
  width: 10px;
  height: 10px;
  border-radius: 50%;
  background: #3a352e;
}

.bar i:first-child {
  background: var(--st-vermilion);
}

.terminal pre {
  margin: 0;
  padding: 20px 22px 24px;
  overflow-x: auto;
  font-family: var(--vp-font-family-mono);
  font-size: 13.5px;
  line-height: 1.7;
  color: var(--st-code-fg);
}

.terminal .p {
  color: var(--st-vermilion);
  user-select: none;
}

.terminal .o {
  color: #8f877a;
}

/* ----------------------------------------------------------- closing --- */

.closing {
  position: relative;
  text-align: center;
  padding: 72px 24px;
  border: 2px dotted var(--vp-c-divider);
  border-radius: 28px;
  overflow: hidden;
}

.closing-dots {
  position: absolute;
  inset: 0;
  background-image: radial-gradient(var(--st-vermilion) 1.6px, transparent 2px);
  background-size: 16px 16px;
  mask-image: radial-gradient(ellipse at 50% 120%, black, transparent 65%);
  opacity: 0.5;
  z-index: -1;
}

.closing h2 {
  margin-bottom: 12px;
}

.closing p {
  margin: 0 auto;
  max-width: 540px;
  font-size: 17px;
  color: var(--vp-c-text-2);
}

/* -------------------------------------------------------- responsive --- */

@media (max-width: 960px) {
  .hero,
  .terminal-wrap {
    grid-template-columns: 1fr;
  }

  .hero {
    padding-top: 40px;
  }

  .hero-art {
    max-width: 320px;
    order: -1;
  }

  .stats {
    grid-template-columns: repeat(2, 1fr);
  }

  .stat:nth-child(3) {
    padding-left: 0;
    border-left: none;
  }

  .stat:nth-child(n + 3) {
    border-top: 1px dotted var(--vp-c-border);
  }

  .grid {
    grid-template-columns: repeat(2, 1fr);
  }

  .flow ol {
    grid-template-columns: repeat(2, 1fr);
  }

  .flow ol::before {
    display: none;
  }
}

@media (max-width: 600px) {
  .grid,
  .flow ol,
  .stats {
    grid-template-columns: 1fr;
  }

  .stat + .stat {
    padding-left: 0;
    border-left: none;
    border-top: 1px dotted var(--vp-c-border);
  }

  .stat-value {
    white-space: normal;
  }
}
</style>
