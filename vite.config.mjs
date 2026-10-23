import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { execFile, spawn } from "node:child_process";
import { access } from "node:fs/promises";
import { constants } from "node:fs";
import { resolve } from "node:path";
import { promisify } from "node:util";

import { guardRequest } from "./scripts/bridge-guard.mjs";

const execFileAsync = promisify(execFile);

/// Hostnames besides loopback the bridge is allowed to answer for. Empty: the
/// dev server must stay on this machine, because `/api/turnout/*` can rewrite
/// every harness config.
const BRIDGE_EXTRA_HOSTS = [];

async function turnoutExecutable() {
  // Windows builds carry the `.exe` suffix, so the same candidate list cannot
  // be used verbatim on every platform.
  const suffix = process.platform === "win32" ? ".exe" : "";
  const candidates = [
    resolve(process.cwd(), `target/release/turnout${suffix}`),
    resolve(process.cwd(), `target/debug/turnout${suffix}`),
  ];
  for (const candidate of candidates) {
    try {
      await access(candidate, constants.X_OK);
      return candidate;
    } catch {
      // Try the next locally built standalone executable.
    }
  }
  throw new Error("Turnout çekirdeği bulunamadı. Uygulama paketi eksik.");
}

async function readJson(request) {
  let body = "";
  for await (const chunk of request) {
    body += chunk;
    if (body.length > 256_000) throw new Error("İstek fazla büyük");
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
    throw new Error("Bilinmeyen harness");
  }
  // CLI profiles and editor injection are deliberately separate operations.
  // Older prototype payloads used two booleans; keep a narrow fallback so a
  // stale tab still works while every new request sends one explicit surface.
  const surface = String(route.surface || (route.vscode ? "vscode" : "cli")).trim().toLowerCase();
  if (!["cli", "vscode"].includes(surface)) {
    throw new Error("Hedef `cli` veya `vscode` olmalı");
  }
  if (!Array.isArray(route.targets) || route.targets.length === 0) {
    throw new Error("En az bir model seçilmeli");
  }

  const targets = [];
  for (const item of route.targets) {
    const target = {
      provider: String(item.provider || "").trim().toLowerCase(),
      model: String(item.model || "").trim(),
    };
    if (!target.provider || !target.model) {
      throw new Error("Sağlayıcı veya model kimliği eksik");
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
    throw new Error("Varsayılan model etkin model listesinde değil");
  }

  // Hybrid keeps the harness's built-in models and appends the selected models
  // from other providers: Claude's stay on Claude's own login (forwarded by the
  // gateway), Codex's are served by the OpenAI connection under their own ids.
  const hybrid = surface === "vscode" && Boolean(route.hybrid);
  if (hybrid) {
    const official = ORIGINAL_LIST_PROVIDER[harness];
    if (!official) throw new Error("Orijinal liste yalnızca Claude Code ve Codex için kullanılabilir");
    if (!targets.some((target) => target.provider !== official)) {
      throw new Error(`Orijinal listeye eklemek için ${official} dışında en az bir model seçin`);
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
      const error = new Error(stderr || stdout || `Turnout ${code} koduyla kapandı`);
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

const turnoutBridge = {
  name: "turnout-local-bridge",
  configureServer(server) {
    server.middlewares.use(turnoutMiddleware);
  },
  configurePreviewServer(server) {
    server.middlewares.use(turnoutMiddleware);
  },
};

async function turnoutMiddleware(request, response, next) {
  const url = new URL(request.url, "http://turnout.local");
  if (!url.pathname.startsWith("/api/turnout/")) return next();

  // The bridge runs the real CLI. Only this machine and this page may drive it.
  const refusal = guardRequest(
    { method: request.method, headers: request.headers },
    { extraHosts: BRIDGE_EXTRA_HOSTS },
  );
  if (refusal) return sendJson(response, refusal.status, { error: refusal.error });

  try {
    const turnout = await turnoutExecutable();

    if (url.pathname === "/api/turnout/providers" && request.method === "GET") {
      return sendJson(response, 200, { providers: await providerList(turnout) });
    }

    if (url.pathname === "/api/turnout/oauth" && request.method === "GET") {
      return sendJson(response, 200, { connections: await oauthList(turnout) });
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
      return sendJson(response, 200, {
        running: routes.some((route) => route.ready),
        routes,
      });
    }

    if (url.pathname === "/api/turnout/rollback" && request.method === "POST") {
      if (request.headers["x-turnout-ui"] !== "1") {
        return sendJson(response, 403, { error: "Geçersiz geri alma isteği" });
      }
      const result = await execFileAsync(turnout, ["rollback"], { maxBuffer: 2_000_000, timeout: 60_000 });
      return sendJson(response, 200, { ok: true, detail: result.stdout.trim() });
    }

    const oauthLoginMatch = url.pathname.match(/^\/api\/turnout\/oauth\/(openai|anthropic)\/login$/);
    if (oauthLoginMatch && request.method === "POST") {
      if (request.headers["x-turnout-ui"] !== "1") {
        return sendJson(response, 403, { error: "Geçersiz OAuth isteği" });
      }
      await execFileAsync(turnout, ["auth", "login", oauthLoginMatch[1]], {
        maxBuffer: 2_000_000,
        timeout: 15_000,
      });
      return sendJson(response, 202, { ok: true });
    }

    if (url.pathname === "/api/turnout/providers" && request.method === "POST") {
      if (request.headers["x-turnout-ui"] !== "1") {
        return sendJson(response, 403, { error: "Geçersiz bağlantı isteği" });
      }
      const input = await readJson(request);
      const id = String(input.id || "").trim().toLowerCase();
      const apiKey = String(input.apiKey || "").trim();
      const baseUrl = String(input.baseUrl || "").trim();
      const dialect = String(input.dialect || "").trim();
      if (!/^[a-z0-9][a-z0-9_-]{1,47}$/.test(id)) throw new Error("Geçerli bir sağlayıcı kimliği gir");
      if (!apiKey) throw new Error("API anahtarı boş olamaz");
      if (baseUrl) new URL(baseUrl);
      if (dialect && !["openai-chat", "openai-responses", "anthropic-messages"].includes(dialect)) {
        throw new Error("Bilinmeyen API protokolü");
      }
      const args = ["providers", "add", id];
      if (baseUrl) args.push("--base-url", baseUrl);
      if (dialect) args.push("--dialect", dialect);
      args.push("--key-stdin");
      await spawnTurnout(turnout, args, `${apiKey}\n`);
      return sendJson(response, 200, { ok: true, providers: await providerList(turnout) });
    }

    if (url.pathname === "/api/turnout/providers/test" && request.method === "POST") {
      if (request.headers["x-turnout-ui"] !== "1") {
        return sendJson(response, 403, { error: "Geçersiz test isteği" });
      }
      const input = await readJson(request);
      const provider = String(input.provider || "").trim().toLowerCase();
      const model = String(input.model || "").trim();
      if (!/^[a-z0-9][a-z0-9_-]{1,47}$/.test(provider) || !model) {
        throw new Error("Test için sağlayıcı ve model gerekli");
      }
      const startedAt = Date.now();
      const result = await execFileAsync(
        turnout,
        ["test", provider, "--model", model],
        { maxBuffer: 2_000_000, timeout: 30_000 },
      );
      return sendJson(response, 200, {
        ok: true,
        latency: Date.now() - startedAt,
        detail: result.stdout.trim(),
      });
    }

    if (url.pathname === "/api/turnout/profiles" && request.method === "GET") {
      const result = await execFileAsync(turnout, ["profiles", "--json"], {
        maxBuffer: 4_000_000,
        timeout: 15_000,
      });
      return sendJson(response, 200, JSON.parse(result.stdout));
    }

    // Backups + the Agent log; both come from the CLI, never from a mock.
    if (url.pathname === "/api/turnout/history" && request.method === "GET") {
      const limit = Math.min(200, Math.max(1, Number(url.searchParams.get("limit")) || 30));
      const result = await execFileAsync(
        turnout,
        ["history", "--json", "--limit", String(limit)],
        { maxBuffer: 8_000_000, timeout: 20_000 },
      );
      return sendJson(response, 200, JSON.parse(result.stdout));
    }

    const providerMatch = url.pathname.match(/^\/api\/turnout\/providers\/([a-z0-9_-]+)$/);
    if (providerMatch && request.method === "DELETE") {
      if (request.headers["x-turnout-ui"] !== "1") {
        return sendJson(response, 403, { error: "Geçersiz bağlantı isteği" });
      }
      await execFileAsync(turnout, ["providers", "remove", providerMatch[1]], { maxBuffer: 2_000_000 });
      return sendJson(response, 200, { ok: true, providers: await providerList(turnout) });
    }

    if (url.pathname !== "/api/turnout/routes/apply") return next();
    if (request.method !== "POST" || request.headers["x-turnout-ui"] !== "1") {
      return sendJson(response, 405, { error: "Geçersiz uygulama isteği" });
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
          `Sağlayıcı \`${provider}\` Turnout'a bağlı değil. Önce Bağlantılar ekranından API anahtarını ekle.`,
        );
      }
      if (!connection.has_key) {
        throw new Error(
          `Sağlayıcı \`${provider}\` için API anahtarı yok. Claude abonelik oturumu Turnout gateway tarafından API anahtarı yerine kullanılamaz.`,
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
      return sendJson(response, 200, {
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

    return sendJson(response, 200, {
      ok: true,
      harness: route.harness,
      providers: [...new Set(route.targets.map((target) => target.provider))],
      models: route.targets.length,
      defaultModel: route.defaultTarget.model,
      surface: "vscode",
      mode: nativeOAuth ? "oauth" : (route.hybrid ? "hybrid" : "gateway"),
      detail: applied.stdout.trim(),
    });
  } catch (error) {
    return sendJson(response, 400, { error: errorDetail(error) });
  }
}

export default defineConfig({
  build: {
    outDir: "dist/client",
  },
  optimizeDeps: {
    include: ["react", "react-dom/client"],
  },
  server: {
    // Loopback only: the bridge behind `/api/turnout/*` executes the real CLI,
    // so nothing on the LAN may reach it.
    host: "127.0.0.1",
    allowedHosts: ["terminal.local"],
    warmup: {
      clientFiles: ["./src/main.jsx"],
    },
  },
  plugins: [react(), turnoutBridge],
});
