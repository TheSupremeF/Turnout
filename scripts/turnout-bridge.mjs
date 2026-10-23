//! The local bridge behind `/api/turnout/*`.
//!
//! Extracted from `vite.config.mjs` so the Vite dev server and the Electron
//! shell run the *same* code. Both front ends must behave identically: this
//! module shells out to the real `turnout` CLI, which may rewrite every harness
//! config on the machine, so it is deliberately single-sourced.
//!
//! `bridgeHandler` is transport-agnostic — it takes a Node `IncomingMessage`
//! and a `ServerResponse`-like object. Vite hands it its own `req`/`res`;
//! Electron serves it from a loopback `http.Server`.

import { execFile, spawn } from "node:child_process";
import { access } from "node:fs/promises";
import { constants } from "node:fs";
import { resolve } from "node:path";
import { promisify } from "node:util";

import { guardRequest } from "./bridge-guard.mjs";

const execFileAsync = promisify(execFile);

/// Hostnames besides loopback the bridge is allowed to answer for. Empty: the
/// bridge must stay on this machine, because `/api/turnout/*` can rewrite
/// every harness config.
export const BRIDGE_EXTRA_HOSTS = [];

/// A packaged app carries the CLI beside itself; a source checkout has it in
/// `target/`. `extraRoots` lets Electron add `process.resourcesPath`.
export async function turnoutExecutable(extraRoots = []) {
  // Windows builds carry the `.exe` suffix, so the same candidate list cannot
  // be used verbatim on every platform.
  const suffix = process.platform === "win32" ? ".exe" : "";
  const roots = [...extraRoots, process.cwd()];
  const candidates = roots.flatMap((root) => [
    resolve(root, `turnout${suffix}`),
    resolve(root, "bin", `turnout${suffix}`),
    resolve(root, `target/release/turnout${suffix}`),
    resolve(root, `target/debug/turnout${suffix}`),
  ]);
  for (const candidate of candidates) {
    try {
      await access(candidate, constants.X_OK);
      return candidate;
    } catch {
      // Try the next locally built standalone executable.
    }
  }
  throw new Error("Turnout core not found. The application bundle is incomplete.");
}

async function readJson(request) {
  let body = "";
  for await (const chunk of request) {
    body += chunk;
    if (body.length > 256_000) throw new Error("Request is too large");
  }
  return JSON.parse(body || "{}");
}

function sendJson(response, status, value) {
  response.statusCode = status;
  response.setHeader("Content-Type", "application/json; charset=utf-8");
  response.end(JSON.stringify(value));
}

const ORIGINAL_LIST_PROVIDER = { claude: "anthropic", codex: "openai" };

function normalizeRoute(route) {
  const harness = String(route.harness || "").trim().toLowerCase();
  if (!["codex", "claude", "opencode"].includes(harness)) {
    throw new Error("Unknown harness");
  }
  // CLI profiles and editor injection are deliberately separate operations.
  // Older prototype payloads used two booleans; keep a narrow fallback so a
  // stale tab still works while every new request sends one explicit surface.
  const surface = String(route.surface || (route.vscode ? "vscode" : "cli")).trim().toLowerCase();
  if (!["cli", "vscode"].includes(surface)) {
    throw new Error("Target must be `cli` or `vscode`");
  }
  if (!Array.isArray(route.targets) || route.targets.length === 0) {
    throw new Error("Select at least one model");
  }

  const targets = [];
  for (const item of route.targets) {
    const target = {
      provider: String(item.provider || "").trim().toLowerCase(),
      model: String(item.model || "").trim(),
    };
    if (!target.provider || !target.model) {
      throw new Error("Provider or model id is missing");
    }
    if (!targets.some((candidate) => candidate.provider === target.provider && candidate.model === target.model)) {
      targets.push(target);
    }
  }

  const defaultTarget = {
    provider: String(route.defaultTarget?.provider || "").trim().toLowerCase(),
    model: String(route.defaultTarget?.model || "").trim(),
  };
  if (!targets.some((target) => target.provider === defaultTarget.provider && target.model === defaultTarget.model)) {
    throw new Error("The default model is not in the enabled model list");
  }

  // Hybrid keeps the harness's built-in models and appends the selected models
  // from other providers: Claude's stay on Claude's own login (forwarded by the
  // gateway), Codex's are served by the OpenAI connection under their own ids.
  const hybrid = surface === "vscode" && Boolean(route.hybrid);
  if (hybrid) {
    const official = ORIGINAL_LIST_PROVIDER[harness];
    if (!official) throw new Error("The original list is only available for Claude Code and Codex");
    if (!targets.some((target) => target.provider !== official)) {
      throw new Error(`Select at least one model outside ${official} to add to the original list`);
    }
  }

  // Rerouting a model to the route's other models on rate-limit/outage is on
  // unless the user turns it off; it never leaves the Turnout gateway.
  const fallback = route.fallback !== false;

  return { harness, targets, defaultTarget, surface, hybrid, fallback };
}

function errorDetail(error) {
  const text = error?.stderr || error?.stdout || error?.message || String(error);
  return String(text).trim().split("\n").filter(Boolean).slice(-2).join(" ");
}

function spawnTurnout(executable, args, stdin = "") {
  return new Promise((resolvePromise, rejectPromise) => {
    const child = spawn(executable, args, { stdio: ["pipe", "pipe", "pipe"] });
    let stdout = "";
    let stderr = "";
    child.stdout.on("data", (chunk) => { stdout += chunk; });
    child.stderr.on("data", (chunk) => { stderr += chunk; });
    child.on("error", rejectPromise);
    child.on("close", (code) => {
      if (code === 0) return resolvePromise({ stdout, stderr });
      const error = new Error(stderr || stdout || `Turnout exited with code ${code}`);
      error.stdout = stdout;
      error.stderr = stderr;
      rejectPromise(error);
    });
    child.stdin.end(stdin);
  });
}

async function providerList(turnout) {
  const result = await execFileAsync(turnout, ["providers", "list", "--json"], { maxBuffer: 2_000_000 });
  return JSON.parse(result.stdout);
}

async function oauthList(turnout) {
  const result = await execFileAsync(turnout, ["auth", "status", "--json"], {
    maxBuffer: 2_000_000,
    timeout: 15_000,
  });
  return JSON.parse(result.stdout);
}

/**
 * Handle one `/api/turnout/*` request.
 *
 * Returns `true` when the request was answered, `false` when it is not ours.
 * `options.executable` skips discovery (the Electron shell resolves the CLI
 * once at startup); `options.onMissingCli` formats the error shown when the
 * binary cannot be found.
 */
export async function bridgeHandler(request, response, options = {}) {
  const url = new URL(request.url, "http://turnout.local");
  if (!url.pathname.startsWith("/api/turnout/")) return false;

  // The bridge runs the real CLI. Only this machine and this page may drive it.
  const refusal = guardRequest(
    { method: request.method, headers: request.headers },
    { extraHosts: options.extraHosts || BRIDGE_EXTRA_HOSTS },
  );
  if (refusal) {
    sendJson(response, refusal.status, { error: refusal.error });
    return true;
  }

  try {
    const turnout = options.executable || (await turnoutExecutable(options.roots || []));

    if (url.pathname === "/api/turnout/providers" && request.method === "GET") {
      sendJson(response, 200, { providers: await providerList(turnout) });
      return true;
    }

    if (url.pathname === "/api/turnout/oauth" && request.method === "GET") {
      sendJson(response, 200, { connections: await oauthList(turnout) });
      return true;
    }

    // The Agent pill reports the real background gateway, never a mock state.
    if (url.pathname === "/api/turnout/agent" && request.method === "GET") {
      let stdout = "";
      try {
        ({ stdout } = await execFileAsync(turnout, ["gateway", "status"], {
          maxBuffer: 2_000_000,
          timeout: 15_000,
        }));
      } catch (error) {
        stdout = error.stdout || "";
      }
      const routes = stdout
        .split("\n")
        .map((line) => line.trim().split(/\s+/))
        .filter((parts) => parts.length >= 3)
        .map(([harness, state, address]) => ({ harness, ready: state === "ready", address }));
      sendJson(response, 200, {
        running: routes.some((route) => route.ready),
        routes,
      });
      return true;
    }

    if (url.pathname === "/api/turnout/rollback" && request.method === "POST") {
      if (request.headers["x-turnout-ui"] !== "1") {
        sendJson(response, 403, { error: "Invalid rollback request" });
        return true;
      }
      const result = await execFileAsync(turnout, ["rollback"], { maxBuffer: 2_000_000, timeout: 60_000 });
      sendJson(response, 200, { ok: true, detail: result.stdout.trim() });
      return true;
    }

    const oauthLoginMatch = url.pathname.match(/^\/api\/turnout\/oauth\/(openai|anthropic)\/login$/);
    if (oauthLoginMatch && request.method === "POST") {
      if (request.headers["x-turnout-ui"] !== "1") {
        sendJson(response, 403, { error: "Invalid OAuth request" });
        return true;
      }
      await execFileAsync(turnout, ["auth", "login", oauthLoginMatch[1]], {
        maxBuffer: 2_000_000,
        timeout: 15_000,
      });
      sendJson(response, 202, { ok: true });
      return true;
    }

    if (url.pathname === "/api/turnout/providers" && request.method === "POST") {
      if (request.headers["x-turnout-ui"] !== "1") {
        sendJson(response, 403, { error: "Invalid connection request" });
        return true;
      }
      const input = await readJson(request);
      const id = String(input.id || "").trim().toLowerCase();
      const apiKey = String(input.apiKey || "").trim();
      const baseUrl = String(input.baseUrl || "").trim();
      const dialect = String(input.dialect || "").trim();
      if (!/^[a-z0-9][a-z0-9_-]{1,47}$/.test(id)) throw new Error("Enter a valid provider id");
      if (!apiKey) throw new Error("The API key cannot be empty");
      if (baseUrl) new URL(baseUrl);
      if (dialect && !["openai-chat", "openai-responses", "anthropic-messages"].includes(dialect)) {
        throw new Error("Unknown API protocol");
      }
      const args = ["providers", "add", id];
      if (baseUrl) args.push("--base-url", baseUrl);
      if (dialect) args.push("--dialect", dialect);
      args.push("--key-stdin");
      await spawnTurnout(turnout, args, `${apiKey}\n`);
      sendJson(response, 200, { ok: true, providers: await providerList(turnout) });
      return true;
    }

    if (url.pathname === "/api/turnout/providers/test" && request.method === "POST") {
      if (request.headers["x-turnout-ui"] !== "1") {
        sendJson(response, 403, { error: "Invalid test request" });
        return true;
      }
      const input = await readJson(request);
      const provider = String(input.provider || "").trim().toLowerCase();
      const model = String(input.model || "").trim();
      if (!/^[a-z0-9][a-z0-9_-]{1,47}$/.test(provider) || !model) {
        throw new Error("A provider and model are required to test");
      }
      const startedAt = Date.now();
      const result = await execFileAsync(
        turnout,
        ["test", provider, "--model", model],
        { maxBuffer: 2_000_000, timeout: 30_000 },
      );
      sendJson(response, 200, {
        ok: true,
        latency: Date.now() - startedAt,
        detail: result.stdout.trim(),
      });
      return true;
    }

    if (url.pathname === "/api/turnout/profiles" && request.method === "GET") {
      const result = await execFileAsync(turnout, ["profiles", "--json"], {
        maxBuffer: 4_000_000,
        timeout: 15_000,
      });
      sendJson(response, 200, JSON.parse(result.stdout));
      return true;
    }

    // Backups + the Agent log; both come from the CLI, never from a mock.
    if (url.pathname === "/api/turnout/history" && request.method === "GET") {
      const limit = Math.min(200, Math.max(1, Number(url.searchParams.get("limit")) || 30));
      const result = await execFileAsync(
        turnout,
        ["history", "--json", "--limit", String(limit)],
        { maxBuffer: 8_000_000, timeout: 20_000 },
      );
      sendJson(response, 200, JSON.parse(result.stdout));
      return true;
    }

    const providerMatch = url.pathname.match(/^\/api\/turnout\/providers\/([a-z0-9_-]+)$/);
    if (providerMatch && request.method === "DELETE") {
      if (request.headers["x-turnout-ui"] !== "1") {
        sendJson(response, 403, { error: "Invalid connection request" });
        return true;
      }
      await execFileAsync(turnout, ["providers", "remove", providerMatch[1]], { maxBuffer: 2_000_000 });
      sendJson(response, 200, { ok: true, providers: await providerList(turnout) });
      return true;
    }

    if (url.pathname !== "/api/turnout/routes/apply") return false;
    if (request.method !== "POST" || request.headers["x-turnout-ui"] !== "1") {
      sendJson(response, 405, { error: "Invalid apply request" });
      return true;
    }

    const route = normalizeRoute(await readJson(request));
    const oauth = await oauthList(turnout);
    const nativeOAuth = (
      route.harness === "codex"
      && route.targets.every((target) => target.provider === "openai")
      && oauth.find((connection) => connection.id === "openai")?.logged_in
    ) || (
      route.harness === "claude"
      && route.targets.every((target) => target.provider === "anthropic")
      && oauth.find((connection) => connection.id === "anthropic")?.logged_in
    );
    const configuredProviders = new Map(
      (await providerList(turnout)).map((provider) => [provider.id, provider]),
    );
    // Anthropic models are served by Claude's own login in hybrid mode, so only
    // the added providers need a Turnout connection and key. Codex's original
    // list is served by the OpenAI connection, which must therefore exist.
    const routedProviders = route.targets
      .map((target) => target.provider)
      .filter((provider) => !(route.hybrid && route.harness === "claude") || provider !== "anthropic");
    if (route.hybrid && route.harness === "codex") routedProviders.push("openai");
    for (const provider of nativeOAuth ? [] : new Set(routedProviders)) {
      const connection = configuredProviders.get(provider);
      if (!connection) {
        throw new Error(
          `Provider \`${provider}\` is not connected to Turnout. Add its API key on the Connections screen first.`,
        );
      }
      if (!connection.has_key) {
        throw new Error(
          `Provider \`${provider}\` has no API key. A Claude subscription session cannot stand in for an API key on the Turnout gateway.`,
        );
      }
    }
    const args = ["plan-targets", route.harness];
    for (const target of route.targets) {
      args.push("--target", `${target.provider}::${target.model}`);
    }
    args.push(
      "--default-target",
      `${route.defaultTarget.provider}::${route.defaultTarget.model}`,
    );

    if (route.surface === "cli") {
      // Official OAuth and custom/mixed routes are both saved as launch-only
      // profiles. Native providers stay direct inside their harness; gateway
      // providers get a private temporary gateway at launch. Neither path
      // mutates the VS Code extension's shared config.
      const profile = `gui-${route.harness}`;
      args.push("--profile", profile);
      if (!route.fallback) args.push("--no-fallback");
      const saved = await execFileAsync(turnout, args, { maxBuffer: 2_000_000 });
      sendJson(response, 200, {
        ok: true,
        harness: route.harness,
        providers: [...new Set(route.targets.map((target) => target.provider))],
        models: route.targets.length,
        defaultModel: route.defaultTarget.model,
        mode: "cli-profile",
        surface: "cli",
        profile,
        command: `turnout run ${route.harness} --profile ${profile}`,
        detail: saved.stdout.trim(),
      });
      return true;
    }

    if (route.hybrid) args.push("--hybrid");
    if (!route.fallback) args.push("--no-fallback");
    await execFileAsync(turnout, args, { maxBuffer: 2_000_000 });
    const applied = await execFileAsync(
      turnout,
      ["apply", "--harness", route.harness],
      { maxBuffer: 2_000_000 },
    );
    if (!nativeOAuth) {
      await execFileAsync(turnout, ["gateway", "install"], { maxBuffer: 2_000_000 });
    }

    sendJson(response, 200, {
      ok: true,
      harness: route.harness,
      providers: [...new Set(route.targets.map((target) => target.provider))],
      models: route.targets.length,
      defaultModel: route.defaultTarget.model,
      surface: "vscode",
      mode: nativeOAuth ? "oauth" : (route.hybrid ? "hybrid" : "gateway"),
      detail: applied.stdout.trim(),
    });
    return true;
  } catch (error) {
    sendJson(response, 400, { error: errorDetail(error) });
    return true;
  }
}

/// Vite plugin wrapper: serves the bridge from the dev and preview servers.
export function turnoutBridgePlugin() {
  const middleware = (request, response, next) => {
    bridgeHandler(request, response)
      .then((handled) => { if (!handled) next(); })
      .catch(next);
  };
  return {
    name: "turnout-local-bridge",
    configureServer(server) {
      server.middlewares.use(middleware);
    },
    configurePreviewServer(server) {
      server.middlewares.use(middleware);
    },
  };
}
