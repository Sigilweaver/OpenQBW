# OpenQBW docs site

[Docusaurus](https://docusaurus.io/) site for OpenQBW, deploying to
<https://sigilweaver.app/openqbw/docs/> via Cloudflare Workers.

The configuration, theme, Docusaurus version, and build tooling come from
[docs-kit](https://github.com/Sigilweaver/docs-kit), pinned by tag in
`package.json`. This directory holds the content (`docs/`), the sidebar
(`sidebars.ts`), and the site options passed to `createConfig` in
`docusaurus.config.ts`.

## Develop

```sh
bun install
bun run dev          # http://localhost:25815/openqbw/docs/
```

## Build and deploy

Cloudflare deploys automatically via the GitHub App on push to `main`.
To verify the build locally (Python 3.10 or later is required for the
output validation that runs after every build):

```sh
bun run build:cloudflare
```

## Upgrade docs-kit

Change the tag in `package.json` and in the `docs-validation` job of
`.github/workflows/ci.yml` to the new release, then run `bun install` and
commit the updated `bun.lock`. See the docs-kit README for details.
