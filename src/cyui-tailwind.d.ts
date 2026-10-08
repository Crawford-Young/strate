// Shim: @crawfordyoung/ui@0.29.1 ships no dist/tailwind/index.d.ts although its exports map
// points at it (tsup `clean: true` on the first entry races the second entry's dts build).
// Remove once the library publishes the declaration.
declare module '@crawfordyoung/ui/tailwind' {
  import type { Config } from 'tailwindcss'
  export const cyUIPreset: Partial<Config>
}
