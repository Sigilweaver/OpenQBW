# OpenQBW docs site

[Docusaurus](https://docusaurus.io/) site for OpenQBW, deploying to
<https://sigilweaver.app/openqbw/docs/> via Cloudflare Workers.

## Develop

```sh
bun install
bun run dev          # http://localhost:25815/openqbw/docs/
```

## Build and deploy

CI auto-deploys on push to `main`. For a local deploy:

```sh
bun run deploy
```
