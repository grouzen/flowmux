import { defineConfig } from 'astro/config';
import sitemap from '@astrojs/sitemap';

export default defineConfig({
  site: 'https://flowmux.dev',
  output: 'static',
  integrations: [sitemap()],
});
