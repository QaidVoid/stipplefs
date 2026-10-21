import { defineConfig } from 'vitepress'

const repo = 'https://github.com/QaidVoid/stipplefs'

export default defineConfig({
  title: 'stipplefs',
  description:
    'A read-only, compressed, deduplicating filesystem image format, with a FUSE mount built for fast application start-up.',
  lang: 'en-US',
  cleanUrls: true,
  lastUpdated: true,
  appearance: true,

  head: [
    ['link', { rel: 'icon', type: 'image/svg+xml', href: '/favicon.svg' }],
    ['meta', { name: 'theme-color', content: '#ff5a36' }],
    ['meta', { property: 'og:type', content: 'website' }],
    ['meta', { property: 'og:title', content: 'stipplefs' }],
    [
      'meta',
      {
        property: 'og:description',
        content: 'Pack a tree into one compressed image. Mount it and start fast.',
      },
    ],
  ],

  markdown: {
    theme: { light: 'vitesse-dark', dark: 'vitesse-dark' },
  },

  themeConfig: {
    logo: '/logo.svg',

    nav: [
      { text: 'Guide', link: '/guide/introduction', activeMatch: '/guide/' },
      { text: 'Reference', link: '/reference/mkstipple', activeMatch: '/reference/' },
      { text: 'Internals', link: '/internals/architecture', activeMatch: '/internals/' },
      { text: 'Specification', link: '/format' },
    ],

    sidebar: {
      '/': [
        {
          text: 'Guide',
          items: [
            { text: 'Introduction', link: '/guide/introduction' },
            { text: 'Quick start', link: '/guide/quick-start' },
            { text: 'Building images', link: '/guide/building-images' },
            { text: 'Mounting', link: '/guide/mounting' },
            { text: 'Hot lists', link: '/guide/hot-lists' },
            { text: 'Embedding images', link: '/guide/embedding' },
            { text: 'Tuning', link: '/guide/tuning' },
            { text: 'Using the library', link: '/guide/library' },
          ],
        },
        {
          text: 'Reference',
          items: [
            { text: 'mkstipple', link: '/reference/mkstipple' },
            { text: 'stipple', link: '/reference/stipple' },
            { text: 'Cargo features', link: '/reference/features' },
          ],
        },
        {
          text: 'Internals',
          items: [
            { text: 'Architecture', link: '/internals/architecture' },
            { text: 'Format specification', link: '/format' },
          ],
        },
      ],
    },

    socialLinks: [{ icon: 'github', link: repo }],

    editLink: {
      pattern: `${repo}/edit/main/docs/:path`,
      text: 'Edit this page',
    },

    search: { provider: 'local' },

    outline: { level: [2, 3], label: 'On this page' },

    footer: {
      message: 'Released under the MIT or Apache 2.0 license, at your option.',
    },
  },
})
