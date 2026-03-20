/**
 * ripgit auth worker — GitHub OAuth example
 *
 * Access model:
 *   Anonymous              → read-only (browse, clone, search)
 *   Authenticated          → read + open issues/PRs on any repo
 *   Authenticated + owner  → write (push, merge, admin) on your own repos
 *
 * Routes handled here (everything else forwarded to ripgit):
 *   GET  /               → public home JSON
 *   GET  /login          → start GitHub OAuth login, ?next= redirect after
 *   GET  /logout         → clear session cookie
 *   GET  /settings       → token management UI (requires login)
 *   POST /settings/tokens                → create access token
 *   POST /settings/tokens/:id/revoke     → revoke access token
 *   GET  /oauth/authorize → OAuth provider flow (programmatic clients)
 *   GET  /oauth/callback  → unified GitHub callback
 *   POST /oauth/token     → token exchange (OAuthProvider internal)
 *   *    /admin/*         → agent management API (requires admin scope)
 *
 * Setup:
 *   1. Create a GitHub OAuth App (https://github.com/settings/applications/new)
 *      Callback URL: https://your-worker.workers.dev/oauth/callback
 *      (local dev:   http://localhost:8787/oauth/callback)
 *   2. wrangler kv namespace create OAUTH_KV  → fill IDs in wrangler.toml
 *   3. wrangler secret put GITHUB_CLIENT_SECRET
 *   4. wrangler secret put SESSION_SECRET  (any random 32+ char string)
 *   5. Set GITHUB_CLIENT_ID in wrangler.toml [vars]
 */

import {
  OAuthProvider,
  type AuthRequest,
  type ResolveExternalTokenInput,
  type ResolveExternalTokenResult,
} from "@cloudflare/workers-oauth-provider";
import { WorkerEntrypoint } from "cloudflare:workers";
import { exchangeCode, fetchGitHubUser, githubAuthorizeUrl } from "./github";
import type { ActorProps, Env } from "./types";

const ALL_SCOPES = [
  "repo:read",
  "repo:write",
  "issue:write",
  "pr:merge",
  "admin",
] as const;

const SESSION_COOKIE = "ripgit_session";
const SESSION_MAX_AGE = 7 * 24 * 3600; // 7 days

// ---------------------------------------------------------------------------
// AdminHandler — WorkerEntrypoint for /admin/* routes (programmatic API).
// ctx.props populated by OAuthProvider after token validation.
// ---------------------------------------------------------------------------

export class AdminHandler extends WorkerEntrypoint<Env> {
  async fetch(request: Request): Promise<Response> {
    const actor = this.ctx.props as ActorProps;
    const url = new URL(request.url);

    if (url.pathname === "/admin/agents") {
      if (request.method === "POST") return this.createAgent(request, actor);
      if (request.method === "GET") return this.listAgents(actor);
      return new Response("Method Not Allowed", { status: 405 });
    }

    return new Response("Not Found", { status: 404 });
  }

  private async createAgent(
    request: Request,
    caller: ActorProps,
  ): Promise<Response> {
    if (!caller.scopes.includes("admin")) {
      return Response.json({ error: "admin scope required" }, { status: 403 });
    }
    let body: { name?: unknown; scopes?: unknown };
    try {
      body = (await request.json()) as { name?: unknown; scopes?: unknown };
    } catch {
      return Response.json({ error: "invalid JSON body" }, { status: 400 });
    }
    const name =
      typeof body.name === "string" && body.name.trim()
        ? body.name.trim()
        : null;
    if (!name) {
      return Response.json({ error: "name is required" }, { status: 400 });
    }
    const requested = Array.isArray(body.scopes)
      ? (body.scopes as unknown[]).filter(
          (s): s is string => typeof s === "string",
        )
      : [...ALL_SCOPES];
    const grantedScopes = requested.filter(
      (s) =>
        ALL_SCOPES.includes(s as (typeof ALL_SCOPES)[number]) &&
        caller.scopes.includes(s),
    );
    const { token, actor } = await createAgentToken(
      this.env,
      name,
      caller,
      grantedScopes,
    );
    return Response.json(
      { token, actorId: actor.actorId, name, scopes: grantedScopes },
      { status: 201 },
    );
  }

  private async listAgents(caller: ActorProps): Promise<Response> {
    if (!caller.scopes.includes("admin")) {
      return Response.json({ error: "admin scope required" }, { status: 403 });
    }
    const agents = await listAgentTokens(this.env, caller.actorId);
    return Response.json({ agents });
  }
}

// ---------------------------------------------------------------------------
// OAuthProvider — default export.
// ---------------------------------------------------------------------------

export default new OAuthProvider<Env>({
  apiRoute: "/admin/",
  apiHandler: AdminHandler,
  defaultHandler: { fetch: mainHandler },
  authorizeEndpoint: "/oauth/authorize",
  tokenEndpoint: "/oauth/token",
  clientRegistrationEndpoint: "/oauth/register",
  scopesSupported: [...ALL_SCOPES],
  accessTokenTTL: 3600,
  refreshTokenTTL: 30 * 86400,
  resolveExternalToken: async ({
    token,
    env,
  }: ResolveExternalTokenInput): Promise<ResolveExternalTokenResult | null> => {
    const raw = await (env as Env).OAUTH_KV.get(`agent:${token}`);
    if (!raw) return null;
    return { props: JSON.parse(raw) as ActorProps };
  },
});

// ---------------------------------------------------------------------------
// mainHandler — resolves actor first, routes, then forwards to ripgit
// ---------------------------------------------------------------------------

async function mainHandler(request: Request, env: Env): Promise<Response> {
  const url = new URL(request.url);

  // Resolve identity up front — available to all routes below
  const actor = await resolveActor(request, env);

  // ── Auth + settings routes ────────────────────────────────────────────────

  if (url.pathname === "/login") return handleLogin(request, env);
  if (url.pathname === "/logout") return handleLogout(request);
  if (url.pathname === "/oauth/authorize")
    return handleAuthorize(request, env);
  if (url.pathname === "/oauth/callback") return handleCallback(request, env);

  if (url.pathname === "/settings") {
    if (!actor) return redirect(`/login?next=/settings`);
    return handleSettings(request, env, actor);
  }
  if (url.pathname === "/settings/tokens" && request.method === "POST") {
    if (!actor) return redirect(`/login?next=/settings`);
    return handleCreateToken(request, env, actor);
  }
  // /settings/tokens/:agentId/revoke
  const revokeMatch = url.pathname.match(
    /^\/settings\/tokens\/([^/]+)\/revoke$/,
  );
  if (revokeMatch && request.method === "POST") {
    if (!actor) return redirect(`/login?next=/settings`);
    // decodeURIComponent because url.pathname preserves %3A rather than
    // normalising it to ':', so "agent%3Auuid" would not match the KV key.
    return handleRevokeToken(env, actor, decodeURIComponent(revokeMatch[1]));
  }

  if (url.pathname === "/" && request.method === "GET") {
    // Logged-in users go straight to their profile page
    if (actor) return redirect(`/${actor.actorName}/`);
    return renderLandingPage(new URL(request.url).origin);
  }

  // ── Everything else → ripgit ──────────────────────────────────────────────
  return forwardToRipgit(request, actor, env);
}

// ---------------------------------------------------------------------------
// Settings — browser UI for token management
// ---------------------------------------------------------------------------

async function handleSettings(
  request: Request,
  env: Env,
  actor: ActorProps,
  newToken?: string,
): Promise<Response> {
  const tokens = await listAgentTokens(env, actor.actorId);
  const origin = new URL(request.url).origin;
  const html = renderSettingsPage(actor.actorName, tokens, origin, newToken);
  return new Response(html, {
    headers: { "Content-Type": "text/html; charset=utf-8" },
  });
}

async function handleCreateToken(
  request: Request,
  env: Env,
  actor: ActorProps,
): Promise<Response> {
  const form = await request.formData();
  const name = ((form.get("name") as string) ?? "").trim();
  if (!name) return redirect("/settings");

  const { token } = await createAgentToken(env, name, actor, [...ALL_SCOPES]);

  // Re-render settings page with the new token shown once
  return handleSettings(request, env, actor, token);
}

async function handleRevokeToken(
  env: Env,
  actor: ActorProps,
  agentId: string,
): Promise<Response> {
  const indexKey = `agent-index:${actor.actorId}:${agentId}`;
  const token = await env.OAUTH_KV.get(indexKey);
  if (token) {
    await Promise.all([
      env.OAUTH_KV.delete(`agent:${token}`),
      env.OAUTH_KV.delete(indexKey),
    ]);
  }
  return redirect("/settings");
}

// ---------------------------------------------------------------------------
// Settings page HTML
// ---------------------------------------------------------------------------

function renderSettingsPage(
  actorName: string,
  tokens: { agentId: string; name: string }[],
  origin: string,
  newToken?: string,
): string {
  const host = origin.replace(/^https?:\/\//, "");
  const esc = (s: string) =>
    s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

  const newTokenBanner = newToken
    ? `<div class="banner banner-success">
        <strong>Token created — copy it now, it won't be shown again</strong>
        <div class="token-value">${esc(newToken)}</div>
        <p class="muted">Anyone with this token can push to your repos. Store it securely.</p>
        <p style="margin-top:12px"><strong>Add as a git remote:</strong></p>
        <pre class="cmd">git remote add origin https://${esc(actorName)}:${esc(newToken)}@${esc(host)}/${esc(actorName)}/REPO-NAME
git push origin main</pre>
      </div>`
    : "";

  const tokenRows =
    tokens.length > 0
      ? `<table>
          <thead><tr><th>Name</th><th></th></tr></thead>
          <tbody>
            ${tokens
              .map(
                (t) => `<tr>
              <td>${esc(t.name)}</td>
              <td class="actions">
                <form method="POST" action="/settings/tokens/${encodeURIComponent(t.agentId)}/revoke">
                  <button class="btn-danger btn-sm" onclick="return confirm('Revoke this token?')">Revoke</button>
                </form>
              </td>
            </tr>`,
              )
              .join("")}
          </tbody>
        </table>`
      : `<p class="muted">No tokens yet.</p>`;

  return `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Settings — ripgit</title>
<style>
  *{margin:0;padding:0;box-sizing:border-box}
  body{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;font-size:14px;color:#1f2328;background:#fff}
  .wrap{max-width:800px;margin:0 auto;padding:40px 20px}
  .topbar{display:flex;justify-content:space-between;align-items:center;margin-bottom:32px;padding-bottom:16px;border-bottom:1px solid #d1d9e0}
  .topbar a{color:#0969da;text-decoration:none}.topbar a:hover{text-decoration:underline}
  h1{font-size:24px;margin-bottom:4px}
  h2{font-size:16px;margin:28px 0 10px;font-weight:600}
  .banner{border:1px solid #d1d9e0;border-radius:6px;padding:16px;margin-bottom:20px}
  .banner-success{background:#dafbe1;border-color:#82cfac}
  .token-value{font-family:ui-monospace,monospace;font-size:13px;background:#fff;border:1px solid #d1d9e0;border-radius:4px;padding:8px 10px;word-break:break-all;margin:10px 0;user-select:all}
  .cmd{font-family:ui-monospace,monospace;font-size:12px;background:#f6f8fa;border:1px solid #d1d9e0;border-radius:4px;padding:10px 14px;margin:8px 0;overflow-x:auto;white-space:pre}
  .form-row{display:flex;gap:8px;align-items:center;margin-top:4px}
  input[type=text]{border:1px solid #d1d9e0;border-radius:6px;padding:5px 10px;font-size:14px;width:260px}
  input[type=text]:focus{outline:none;border-color:#0969da;box-shadow:0 0 0 3px rgba(9,105,218,.1)}
  .btn{background:#1f883d;color:#fff;border:none;border-radius:6px;padding:5px 14px;cursor:pointer;font-size:14px}
  .btn:hover{background:#1a7f37}
  .btn-danger{background:#cf222e;color:#fff;border:none;border-radius:6px;cursor:pointer}
  .btn-sm{padding:3px 10px;font-size:13px}
  .btn-danger:hover{background:#a40e26}
  table{width:100%;border-collapse:collapse;margin-top:4px}
  td,th{text-align:left;padding:8px 12px;border-bottom:1px solid #d1d9e0;font-size:14px}
  th{font-weight:600;background:#f6f8fa}
  .actions{text-align:right}
  .muted{color:#656d76;font-size:13px}
  p{line-height:1.5}
</style>
</head>
<body>
<div class="wrap">
  <div class="topbar">
    <span><a href="/">ripgit</a> / Settings</span>
    <span>Signed in as <strong>${esc(actorName)}</strong> &nbsp;·&nbsp; <a href="/logout">Sign out</a></span>
  </div>

  ${newTokenBanner}

  <h1>Access Tokens</h1>
  <p class="muted">Tokens let you push to your repos from the command line without a browser.</p>

  <h2>Create token</h2>
  <form method="POST" action="/settings/tokens">
    <div class="form-row">
      <input type="text" name="name" placeholder="Token name (e.g. laptop, deploy-key)" required autocomplete="off">
      <button class="btn" type="submit">Generate</button>
    </div>
  </form>

  <h2>Push a new repo</h2>
  <p class="muted">Repos are created on first push. Pick any name:</p>
  <pre class="cmd">cd my-project
git init
git add .
git commit -m "initial commit"
git remote add origin https://${esc(actorName)}:TOKEN@${esc(host)}/${esc(actorName)}/my-project
git push origin main</pre>
  <p class="muted" style="margin-top:6px">Replace <code>TOKEN</code> with the token you generate above. Replace <code>my-project</code> with your repo name.</p>

  <h2>Active tokens</h2>
  ${tokenRows}
</div>
</body>
</html>`;
}

// ---------------------------------------------------------------------------
// Landing page (shown at / when not logged in)
// ---------------------------------------------------------------------------

function renderLandingPage(_origin: string): Response {
  const html = `<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>ripgit</title>
<style>
  *{margin:0;padding:0;box-sizing:border-box}
  body{font-family:-apple-system,BlinkMacSystemFont,"Segoe UI",sans-serif;background:#fff;color:#1f2328;display:flex;flex-direction:column;min-height:100vh}
  header{padding:16px 24px;border-bottom:1px solid #d1d9e0;display:flex;align-items:center}
  header .logo{font-weight:700;font-size:16px;color:#1f2328;text-decoration:none}
  main{flex:1;display:flex;flex-direction:column;align-items:center;justify-content:center;padding:60px 24px;text-align:center}
  h1{font-size:40px;font-weight:700;letter-spacing:-1px;margin-bottom:16px}
  .tagline{font-size:18px;color:#656d76;margin-bottom:40px;max-width:480px;line-height:1.5}
  .signin-btn{display:inline-flex;align-items:center;gap:10px;background:#1f2328;color:#fff;border:none;border-radius:6px;padding:12px 24px;font-size:16px;font-weight:500;text-decoration:none;cursor:pointer}
  .signin-btn:hover{background:#393f47}
  .signin-btn svg{width:20px;height:20px;fill:#fff}
  .features{display:flex;gap:32px;margin-top:64px;flex-wrap:wrap;justify-content:center}
  .feature{text-align:left;max-width:200px}
  .feature h3{font-size:14px;font-weight:600;margin-bottom:4px}
  .feature p{font-size:13px;color:#656d76;line-height:1.5}
  footer{padding:24px;text-align:center;font-size:12px;color:#656d76;border-top:1px solid #d1d9e0}
  footer a{color:#656d76}
</style>
</head>
<body>
<header>
  <a href="/" class="logo">ripgit</a>
</header>
<main>
  <h1>ripgit</h1>
  <p class="tagline">A lightweight self-hosted Git server running on Cloudflare Durable Objects. Fast, searchable, yours.</p>
  <a href="/login" class="signin-btn">
    <svg viewBox="0 0 16 16"><path d="M8 0C3.58 0 0 3.58 0 8c0 3.54 2.29 6.53 5.47 7.59.4.07.55-.17.55-.38 0-.19-.01-.82-.01-1.49-2.01.37-2.53-.49-2.69-.94-.09-.23-.48-.94-.82-1.13-.28-.15-.68-.52-.01-.53.63-.01 1.08.58 1.23.82.72 1.21 1.87.87 2.33.66.07-.52.28-.87.51-1.07-1.78-.2-3.64-.89-3.64-3.95 0-.87.31-1.59.82-2.15-.08-.2-.36-1.02.08-2.12 0 0 .67-.21 2.2.82.64-.18 1.32-.27 2-.27.68 0 1.36.09 2 .27 1.53-1.04 2.2-.82 2.2-.82.44 1.1.16 1.92.08 2.12.51.56.82 1.27.82 2.15 0 3.07-1.87 3.75-3.65 3.95.29.25.54.73.54 1.48 0 1.07-.01 1.93-.01 2.2 0 .21.15.46.55.38A8.013 8.013 0 0016 8c0-4.42-3.58-8-8-8z"/></svg>
    Sign in with GitHub
  </a>
  <div class="features">
    <div class="feature">
      <h3>Git-compatible</h3>
      <p>Works with any standard git client. Push and clone with the URLs you already know.</p>
    </div>
    <div class="feature">
      <h3>Built-in search</h3>
      <p>Full-text search across all your code and commit history, powered by SQLite FTS5.</p>
    </div>
    <div class="feature">
      <h3>Edge-hosted</h3>
      <p>Runs on Cloudflare Durable Objects. No servers to manage, globally distributed.</p>
    </div>
  </div>
</main>
<footer>
  ripgit &mdash; <a href="https://github.com/deathbyknowledge/ripgit">open source</a>
</footer>
</body>
</html>`;
  return new Response(html, {
    headers: { "Content-Type": "text/html; charset=utf-8" },
  });
}

// ---------------------------------------------------------------------------
// GitHub OAuth flows
// ---------------------------------------------------------------------------

async function handleLogin(request: Request, env: Env): Promise<Response> {
  const next = new URL(request.url).searchParams.get("next") ?? "/";
  const state = crypto.randomUUID();
  await env.OAUTH_KV.put(
    `state:${state}`,
    JSON.stringify({ type: "login", next }),
    { expirationTtl: 600 },
  );
  const callbackUrl = new URL("/oauth/callback", request.url).toString();
  return Response.redirect(
    githubAuthorizeUrl(env.GITHUB_CLIENT_ID, state, callbackUrl),
    302,
  );
}

async function handleAuthorize(request: Request, env: Env): Promise<Response> {
  const oauthReq = await env.OAUTH_PROVIDER.parseAuthRequest(request);
  const state = crypto.randomUUID();
  await env.OAUTH_KV.put(
    `state:${state}`,
    JSON.stringify({ type: "oauth", oauthReq }),
    { expirationTtl: 600 },
  );
  const callbackUrl = new URL("/oauth/callback", request.url).toString();
  return Response.redirect(
    githubAuthorizeUrl(env.GITHUB_CLIENT_ID, state, callbackUrl),
    302,
  );
}

async function handleCallback(request: Request, env: Env): Promise<Response> {
  const url = new URL(request.url);
  const code = url.searchParams.get("code");
  const state = url.searchParams.get("state");

  if (!code || !state) {
    return new Response("Missing code or state", { status: 400 });
  }

  const stateRaw = await env.OAUTH_KV.get(`state:${state}`);
  if (!stateRaw) {
    return new Response("Invalid or expired state — please try again", {
      status: 400,
    });
  }
  await env.OAUTH_KV.delete(`state:${state}`);

  type StateData =
    | { type: "login"; next: string }
    | { type: "oauth"; oauthReq: AuthRequest };

  const stateData = JSON.parse(stateRaw) as StateData;
  const callbackUrl = new URL("/oauth/callback", request.url).toString();

  let githubToken: string;
  try {
    githubToken = await exchangeCode(code, env, callbackUrl);
  } catch (err) {
    return new Response(`GitHub auth failed: ${(err as Error).message}`, {
      status: 400,
    });
  }

  const user = await fetchGitHubUser(githubToken);

  if (stateData.type === "login") {
    const actor: ActorProps = {
      actorId: `github:${user.id}`,
      actorName: user.login,
      actorKind: "user",
      scopes: [...ALL_SCOPES],
    };
    const sessionValue = await createSession(actor, env.SESSION_SECRET);
    return new Response(null, {
      status: 302,
      headers: {
        Location: stateData.next,
        "Set-Cookie": `${SESSION_COOKIE}=${encodeURIComponent(sessionValue)}; HttpOnly; SameSite=Lax; Path=/; Max-Age=${SESSION_MAX_AGE}`,
      },
    });
  } else {
    const { redirectTo } = await env.OAUTH_PROVIDER.completeAuthorization({
      request: stateData.oauthReq,
      userId: `github:${user.id}`,
      metadata: { githubLogin: user.login },
      scope: stateData.oauthReq.scope,
      props: {
        actorId: `github:${user.id}`,
        actorName: user.login,
        actorKind: "user",
        scopes: stateData.oauthReq.scope,
      } satisfies ActorProps,
    });
    return Response.redirect(redirectTo, 302);
  }
}

function handleLogout(request: Request): Response {
  const next = new URL(request.url).searchParams.get("next") ?? "/";
  return new Response(null, {
    status: 302,
    headers: {
      Location: next,
      "Set-Cookie": `${SESSION_COOKIE}=; HttpOnly; SameSite=Lax; Path=/; Max-Age=0`,
    },
  });
}

// ---------------------------------------------------------------------------
// KV helpers for agent tokens
// ---------------------------------------------------------------------------

async function createAgentToken(
  env: Env,
  name: string,
  caller: ActorProps,
  scopes: string[],
): Promise<{ token: string; actor: ActorProps }> {
  const token = generateToken();
  const agentId = `agent:${crypto.randomUUID()}`;
  const actor: ActorProps = {
    actorId: agentId,
    actorName: name,
    actorKind: "agent",
    ownerActorId: caller.actorId,
    ownerActorName: caller.actorName, // caller's GitHub username — used for repo ownership checks
    scopes,
  };
  await env.OAUTH_KV.put(`agent:${token}`, JSON.stringify(actor));
  await env.OAUTH_KV.put(`agent-index:${caller.actorId}:${agentId}`, token);
  return { token, actor };
}

async function listAgentTokens(
  env: Env,
  actorId: string,
): Promise<{ agentId: string; name: string }[]> {
  const prefix = `agent-index:${actorId}:`;
  const list = await env.OAUTH_KV.list({ prefix });
  const results = await Promise.all(
    list.keys.map(async (k) => {
      const agentId = k.name.slice(prefix.length);
      const token = await env.OAUTH_KV.get(k.name);
      if (!token) return null;
      const raw = await env.OAUTH_KV.get(`agent:${token}`);
      if (!raw) return null;
      const a = JSON.parse(raw) as ActorProps;
      return { agentId, name: a.actorName };
    }),
  );
  return results.filter((r): r is { agentId: string; name: string } => r !== null);
}

// ---------------------------------------------------------------------------
// Session cookie helpers — HMAC-SHA256 signed, stateless
// ---------------------------------------------------------------------------

async function createSession(
  actor: ActorProps,
  secret: string,
): Promise<string> {
  const payload = btoa(JSON.stringify(actor));
  const key = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["sign"],
  );
  const sig = await crypto.subtle.sign(
    "HMAC",
    key,
    new TextEncoder().encode(payload),
  );
  const sigHex = Array.from(new Uint8Array(sig))
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
  return `${payload}.${sigHex}`;
}

async function verifySession(
  value: string,
  secret: string,
): Promise<ActorProps | null> {
  const dot = value.lastIndexOf(".");
  if (dot < 0) return null;
  const payload = value.slice(0, dot);
  const sigHex = value.slice(dot + 1);
  let sigBytes: Uint8Array;
  try {
    const pairs = sigHex.match(/.{2}/g) ?? [];
    sigBytes = Uint8Array.from(pairs.map((h) => parseInt(h, 16)));
  } catch {
    return null;
  }
  const key = await crypto.subtle.importKey(
    "raw",
    new TextEncoder().encode(secret),
    { name: "HMAC", hash: "SHA-256" },
    false,
    ["verify"],
  );
  const valid = await crypto.subtle.verify(
    "HMAC",
    key,
    sigBytes,
    new TextEncoder().encode(payload),
  );
  if (!valid) return null;
  try {
    return JSON.parse(atob(payload)) as ActorProps;
  } catch {
    return null;
  }
}

function getSessionCookie(request: Request): string | null {
  const cookies = request.headers.get("Cookie") ?? "";
  const match = cookies.match(/(?:^|;\s*)ripgit_session=([^;]+)/);
  return match ? decodeURIComponent(match[1]) : null;
}

// ---------------------------------------------------------------------------
// Identity resolution — returns null for anonymous, never blocks
// ---------------------------------------------------------------------------

async function resolveActor(
  request: Request,
  env: Env,
): Promise<ActorProps | null> {
  const token = extractToken(request);
  if (token) {
    const agentRaw = await env.OAUTH_KV.get(`agent:${token}`);
    if (agentRaw) return JSON.parse(agentRaw) as ActorProps;
    const tokenData = await env.OAUTH_PROVIDER.unwrapToken<ActorProps>(token);
    if (tokenData && tokenData.expiresAt > Math.floor(Date.now() / 1000)) {
      return tokenData.grant.props;
    }
  }
  const cookieValue = getSessionCookie(request);
  if (cookieValue) {
    return verifySession(cookieValue, env.SESSION_SECRET);
  }
  return null;
}

// ---------------------------------------------------------------------------
// Forward to ripgit
// ---------------------------------------------------------------------------

function forwardToRipgit(
  request: Request,
  actor: ActorProps | null,
  env: Env,
): Promise<Response> {
  const headers = new Headers(request.headers);
  if (actor) {
    // For ownership checks in ripgit, what matters is the GitHub username of the
    // person who owns the repos. For agents, that's ownerActorName, not actorName
    // (actorName is the token's display name, e.g. "laptop").
    const ownerName =
      actor.actorKind === "agent" && actor.ownerActorName
        ? actor.ownerActorName
        : actor.actorName;

    headers.set("X-Ripgit-Actor-Id", actor.actorId);
    headers.set("X-Ripgit-Actor-Name", ownerName);        // GitHub username for ownership
    headers.set("X-Ripgit-Actor-Display-Name", actor.actorName); // token name for audit/display
    headers.set("X-Ripgit-Actor-Kind", actor.actorKind);
    headers.set("X-Ripgit-Actor-Scopes", actor.scopes.join(","));
    if (actor.ownerActorId) {
      headers.set("X-Ripgit-Actor-Owner", actor.ownerActorId);
    }
  }
  headers.delete("Authorization");
  headers.delete("Cookie");
  return env.RIPGIT.fetch(new Request(request, { headers }));
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function extractToken(request: Request): string | null {
  const auth = request.headers.get("Authorization") ?? "";
  if (auth.startsWith("Bearer ")) {
    const t = auth.slice(7).trim();
    return t || null;
  }
  if (auth.startsWith("Basic ")) {
    try {
      const decoded = atob(auth.slice(6));
      const colon = decoded.indexOf(":");
      if (colon >= 0) {
        const t = decoded.slice(colon + 1);
        return t || null;
      }
    } catch {
      /* malformed base64 */
    }
  }
  return null;
}

/**
 * Redirect to a relative or absolute URL.
 * Response.redirect() only accepts absolute URLs in Cloudflare Workers,
 * so use this helper for any relative paths.
 */
function redirect(location: string, status = 302): Response {
  return new Response(null, { status, headers: { Location: location } });
}

function generateToken(): string {
  const bytes = new Uint8Array(32);
  crypto.getRandomValues(bytes);
  return Array.from(bytes)
    .map((b) => b.toString(16).padStart(2, "0"))
    .join("");
}
