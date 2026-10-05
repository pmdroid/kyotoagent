// The docs site is Astro with Starlight. The landing page is Starlight's
// splash template at src/content/docs/index.mdx, so the front page and the
// docs are one build and one deploy.
import { defineConfig } from 'astro/config';
import starlight from '@astrojs/starlight';
import { satteri } from '@astrojs/markdown-satteri';
import docLinks from './src/doc-links.mjs';

// Serve from the site root, never a sub-path. Starlight prefixes its own
// navigation with `base`, but it does not prefix links written in Markdown
// (e.g. `/cli/`), so a sub-path deploy would break every in-page link.
// `SITE_URL` only sets canonical and sitemap URLs.
const site = process.env.SITE_URL || 'https://kyotoagent.dev';

// The product is Kyoto Agent; the crate, binary, socket, and config use
// `kyotoagent`. Keep those names in the title and CLI prose.
export default defineConfig({
  site,
  trailingSlash: 'always',
  markdown: { processor: satteri({ mdastPlugins: [docLinks] }) },
  redirects: Object.fromEntries(
    ['getting-started', 'cli', 'tui', 'slash', 'model', 'tools', 'server', 'hooks', 'sessions', 'development', 'screenshots'].map(
      (slug) => [`/${slug}/`, `/docs/${slug}/`]
    )
  ),
  integrations: [
    starlight({
      title: 'Kyoto Agent',
      disable404Route: true,
      description:
        'The agent that focuses on what you build, not what it does.',
      logo: {
        src: '../assets/kyoto-logo.png',
        alt: 'Kyoto Agent',
        replacesTitle: true,
      },
      customCss: ['./src/styles/docs.css'],
      components: { Head: './src/components/DocsHead.astro' },
      social: [
        {
          icon: 'github',
          label: 'GitHub',
          href: 'https://github.com/pmdroid/kyotoagent',
        },
      ],
      editLink: {
        baseUrl: 'https://github.com/pmdroid/kyotoagent/edit/main/site/',
      },
      sidebar: [
        {
          label: 'Start',
          items: [{ label: 'Documentation', slug: 'docs' }, { label: 'Getting started', slug: 'docs/getting-started' }],
        },
        {
          label: 'Driving it',
          items: [
            { label: 'CLI', slug: 'docs/cli' },
            { label: 'TUI', slug: 'docs/tui' },
            { label: 'Slash commands', slug: 'docs/slash' },
          ],
        },
        {
          label: 'How it works',
          items: [
            { label: 'The model', slug: 'docs/model' },
            { label: 'Tools and the gate', slug: 'docs/tools' },
            { label: 'The server', slug: 'docs/server' },
            { label: 'Hooks', slug: 'docs/hooks' },
            { label: 'The session log', slug: 'docs/sessions' },
            { label: 'Internals', slug: 'docs/internals' },
          ],
        },
        {
          label: 'Develop',
          items: [
            { label: 'Building and tests', slug: 'docs/development' },
            { label: 'Screenshots', slug: 'docs/screenshots' },
            { label: 'Website', slug: 'docs/website' },
          ],
        },
      ],
    }),
  ],
});
