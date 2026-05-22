# OpenQBW docs site

[Docusaurus](https://docusaurus.io/) site for OpenQBW, deploying to
<https://sigilweaver.app/openqbw/docs/> via Cloudflare Workers.

## Develop

```sh
bun install
bun run dev          # http://localhost:25815/openqbw/docs/
```

## Build and deploy

Cloudflare deploys automatically via the GitHub App on push to `main`.
To verify the build locally:

```sh
bun run build:cloudflare
```
