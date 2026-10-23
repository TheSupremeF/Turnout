//! Access rules for the development bridge (`/api/turnout/*`).
//!
//! The bridge shells out to the real CLI, so anything that can reach it can
//! rewrite every harness config on this machine. Two failures are possible and
//! both have been seen in the wild:
//!
//! - The dev server binds every interface and the API answers anyone on the
//!   LAN. Binding loopback and checking `Host` closes that.
//! - Any page the user visits can POST here. A custom header alone is *not*
//!   CSRF protection: Vite answers the preflight with
//!   `Access-Control-Allow-Headers: x-turnout-ui`, so a cross-origin page can
//!   send it; the browser only hides the response, but the write still lands.
//!   Only an `Origin` allowlist stops it.
//!
//! Kept in its own module so it can be unit tested without starting Vite.

const LOOPBACK_HOSTS = new Set(["127.0.0.1", "::1", "localhost"]);

/** Hostnames the bridge may be reached through. */
export function isLoopbackHost(host) {
  const name = stripPort(host).toLowerCase();
  if (LOOPBACK_HOSTS.has(name)) return true;
  // 127.0.0.0/8 is loopback, not just 127.0.0.1.
  return /^127\.\d{1,3}\.\d{1,3}\.\d{1,3}$/.test(name);
}

function stripPort(host) {
  const value = String(host || "").trim();
  if (value.startsWith("[")) {
    const end = value.indexOf("]");
    return end === -1 ? value : value.slice(1, end);
  }
  return value.replace(/:\d+$/, "");
}

/** Origins allowed to drive the bridge: this machine, over http, any port. */
export function isAllowedOrigin(origin, extraHosts = []) {
  let parsed;
  try {
    parsed = new URL(origin);
  } catch {
    return false;
  }
  if (parsed.protocol !== "http:" && parsed.protocol !== "https:") return false;
  if (isLoopbackHost(parsed.hostname)) return true;
  return extraHosts.map((host) => stripPort(host).toLowerCase()).includes(parsed.hostname.toLowerCase());
}

/**
 * Decide whether a request may reach the CLI. Returns `null` when allowed, or
 * a `{ status, error }` to send back.
 *
 * A missing `Origin` is allowed: curl, the TUI and the test suite are not
 * browsers. Every cross-origin browser POST carries one, so an allowlist still
 * covers the attack that matters.
 */
export function guardRequest({ method = "GET", headers = {} }, options = {}) {
  const extraHosts = options.extraHosts || [];
  const host = headers.host || "";

  // A request addressed to a non-loopback name is someone else on the network
  // (or a DNS-rebinding attempt). Refuse it whatever the Origin says.
  if (host && !isLoopbackHost(host) && !extraHosts.map((h) => stripPort(h).toLowerCase()).includes(stripPort(host).toLowerCase())) {
    return { status: 403, error: "Turnout köprüsüne yalnızca bu makineden erişilebilir" };
  }

  const origin = headers.origin;
  if (origin && !isAllowedOrigin(origin, extraHosts)) {
    return { status: 403, error: "Bu kökenden gelen istekler reddedildi" };
  }

  const mutating = !["GET", "HEAD", "OPTIONS"].includes(String(method).toUpperCase());
  if (mutating && headers["x-turnout-ui"] !== "1") {
    return { status: 403, error: "Geçersiz istek" };
  }
  return null;
}
