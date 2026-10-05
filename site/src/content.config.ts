// Starlight's docs and UI-translation strings are two Astro content
// collections. `docsLoader` reads src/content/docs; `i18nLoader` reads
// optional overrides in src/content/i18n. Both are declared so the build does
// not warn about a collection that has no loader.
import { defineCollection } from 'astro:content';
import { glob } from 'astro/loaders';
import { i18nLoader } from '@astrojs/starlight/loaders';
import { docsSchema, i18nSchema } from '@astrojs/starlight/schema';

export const collections = {
  docs: defineCollection({
    loader: glob({
      base: '../docs',
      pattern: '*.md',
      generateId: ({ entry }) => entry === 'README.md' ? 'docs' : `docs/${entry.replace(/\.md$/, '')}`,
    }),
    schema: docsSchema(),
  }),
  i18n: defineCollection({ loader: i18nLoader(), schema: i18nSchema() }),
};
