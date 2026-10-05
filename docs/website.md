---
title: Working on the website
description: Run, edit, and build the Astro landing page and Starlight documentation.
editUrl: https://github.com/pmdroid/kyotoagent/edit/main/docs/website.md
---

The website lives in `site/` and uses Astro with Starlight. Install Node.js
22.12 or later, then run these commands from the repository root:

```sh
cd site
npm ci
npm run dev
```

Open the local URL printed by Astro.

## Content and routes

The landing page is `site/src/pages/index.astro`. Technical guides live in the
repository's `docs/` folder. Astro reads its top-level Markdown files directly;
`docs/README.md` becomes `/docs/` and `docs/cli.md` becomes `/docs/cli/`.

Use relative Markdown links such as `[Command line](cli.md)` between guides.
The site converts them to website URLs during the build, and GitHub opens the
same links as files. Each guide sets its GitHub edit URL in its frontmatter.

The sidebar lives in `site/astro.config.mjs`. Brand artwork lives in `assets/`.
The site uses the supplied Shiba Sakura logo for web images, the square favicon,
and the 1200 × 630 PNG social preview in `assets/kyoto-social.png`.
`site/src/components/Metadata.astro` sets Open Graph and X cards on the landing
page and each guide. The landing page also includes WebSite structured data.

## Check and build

```sh
npm run check
npm run build
npm run preview
```

The production build writes `site/dist/`, including the documentation search
index. Preview the production build to check search.

## Deploy

Serve `site/dist/` from the domain root at `https://kyotoagent.dev`. The build
uses that domain for canonical URLs, social image URLs, robots.txt, and the sitemap.
Set `SITE_URL` when building for a different domain:

```sh
SITE_URL=https://kyotoagent.dev npm run build
```

In a hosting provider, set the build directory to `site`, the build command
to `npm ci && npm run build`, and the output directory to `dist`.
