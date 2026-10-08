import { cyUIPreset } from '@crawfordyoung/ui/tailwind'
import type { Config } from 'tailwindcss'

export default {
  presets: [cyUIPreset as Config],
  darkMode: 'class',
  content: ['./index.html', './src/**/*.{ts,tsx}'],
} satisfies Config
