import type { Theme } from 'vitepress'
import DefaultTheme from 'vitepress/theme'

import '@fontsource-variable/bricolage-grotesque'
import '@fontsource-variable/instrument-sans'
import '@fontsource-variable/jetbrains-mono'
import './style.css'

import Home from './components/Home.vue'
import StippleCube from './components/StippleCube.vue'

export default {
  extends: DefaultTheme,
  enhanceApp({ app }) {
    app.component('Home', Home)
    app.component('StippleCube', StippleCube)
  },
} satisfies Theme
