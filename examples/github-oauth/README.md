# GitHub OAuth Auth Worker

This example Worker sits in front of ripgit, handles GitHub OAuth, issues browser session cookies, mints long-lived tokens, and forwards trusted `X-Ripgit-Actor-*` headers to the main ripgit Worker through a Service Binding.

## What It Does

- `GET /` - landing page for browsers plus text mode for curl/agents
- `GET /settings` - token management page after sign-in plus text mode for curl/agents
- `GET /login` / `GET /logout` - browser login/logout flow
- `GET /oauth/authorize` / `POST /oauth/token` - OAuth provider flow for programmatic clients
- `POST /settings/tokens` - create a long-lived token
- `POST /settings/tokens/:id/revoke` - revoke a long-lived token

Everything else is forwarded to ripgit.

## Required Bindings And Secrets

Set these in `wrangler.toml` or as Worker secrets:

- `GITHUB_CLIENT_ID` - GitHub OAuth App client ID (`[vars]`)
- `GITHUB_CLIENT_SECRET` - GitHub OAuth App client secret (`wrangler secret put GITHUB_CLIENT_SECRET`)
- `SESSION_SECRET` - random 32+ character secret for signing browser sessions (`wrangler secret put SESSION_SECRET`)
- `OAUTH_KV` - KV namespace used for OAuth state, issued tokens, and token indexes
- `RIPGIT` - Service Binding that points at the main ripgit Worker

`workers-oauth-provider` also injects the `OAUTH_PROVIDER` helper at runtime.

## GitHub OAuth App Setup

Create a GitHub OAuth App at <https://github.com/settings/applications/new>.

- Homepage URL: your deployed auth worker URL, for example `https://git-auth.example.workers.dev`
- Authorization callback URL: `https://git-auth.example.workers.dev/oauth/callback`
- Local dev callback URL: `http://localhost:8787/oauth/callback`

## Local Development

From the repo root:

```bash
cd examples/github-oauth
npm install
npm run dev:full
```

That runs:

- the auth worker on `http://localhost:8787`
- the main ripgit Worker through the local Service Binding declared in `wrangler.toml`

Then:

1. Visit `http://localhost:8787`
2. Sign in with GitHub
3. Open `http://localhost:8787/settings`
4. Generate a token
5. Push a repo with that token

Example push:

```bash
git remote add origin http://USERNAME:TOKEN@localhost:8787/USERNAME/my-project
git push origin main
```

## Deployment

Create the KV namespace and fill the IDs into `examples/github-oauth/wrangler.toml`:

```bash
wrangler kv namespace create OAUTH_KV
wrangler kv namespace create OAUTH_KV --preview
```

Set the secrets:

```bash
wrangler secret put GITHUB_CLIENT_SECRET
wrangler secret put SESSION_SECRET
```

Deploy ripgit first, then the auth worker:

```bash
wrangler deploy
cd examples/github-oauth
wrangler deploy
```

Make sure the `[[services]]` binding in `examples/github-oauth/wrangler.toml` points at the deployed ripgit Worker name.

After deployment, update the GitHub OAuth App callback URL to your deployed auth worker URL.

## Text Mode

The auth worker landing page and `/settings` support the same text-mode negotiation as ripgit repo pages:

```bash
curl -H 'Accept: text/markdown' https://your-auth-worker.example/
curl -H 'Accept: text/plain' https://your-auth-worker.example/settings
curl 'https://your-auth-worker.example/settings?format=md'
```

- `Accept: text/markdown` returns markdown
- `Accept: text/plain` returns plain text
- `?format=md` and `?format=text` work when you can't keep headers attached while following links

The text pages explain what the auth worker does, which paths are available, and which POST actions require an authenticated session.
