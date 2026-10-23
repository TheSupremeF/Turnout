//! Proves the development bridge refuses cross-origin and network access.
//!
//! The bridge shells out to the real `turnout` CLI, so a bypass here is a
//! local-privilege issue: any page you visit could rewrite every harness config
//! on the machine. These cases are the exact attack shapes, not abstractions.

import assert from "node:assert/strict";
import { test } from "node:test";

import { guardRequest, isAllowedOrigin, isLoopbackHost } from "../scripts/bridge-guard.mjs";

const local = { host: "127.0.0.1:5173", origin: "http://127.0.0.1:5173", "x-turnout-ui": "1" };

test("the UI's own requests pass", () => {
  assert.equal(guardRequest({ method: "POST", headers: local }), null);
  assert.equal(guardRequest({ method: "GET", headers: { host: "localhost:5173" } }), null);
});

test("a page on another origin cannot drive the CLI", () => {
  // The custom header is not CSRF protection: Vite answers the preflight with
  // `Access-Control-Allow-Headers: x-turnout-ui`, so the browser sends the
  // request and only hides the response. The write would still land.
  const refusal = guardRequest({
    method: "POST",
    headers: { ...local, origin: "https://evil.example" },
  });
  assert.equal(refusal?.status, 403);
});

test("the same machine on another port is still this machine", () => {
  // A second dev server, the TUI's preview, an ssh-forwarded port: all local.
  assert.equal(guardRequest({ method: "POST", headers: { ...local, origin: "http://localhost:3000" } }), null);
  assert.equal(guardRequest({ method: "POST", headers: { ...local, origin: "http://127.0.0.1:8080" } }), null);
});

test("a LAN address is refused even with the right header", () => {
  // Before the fix the dev server bound 0.0.0.0, so any device on the wifi
  // could POST here. The Host check is what closes it.
  const refusal = guardRequest({
    method: "POST",
    headers: { host: "192.168.0.183:5173", origin: "http://192.168.0.183:5173", "x-turnout-ui": "1" },
  });
  assert.equal(refusal?.status, 403);
});

test("DNS rebinding is refused", () => {
  const refusal = guardRequest({
    method: "GET",
    headers: { host: "evil.example:5173", origin: "http://evil.example:5173" },
  });
  assert.equal(refusal?.status, 403);
});

test("a mutating request without the header is refused", () => {
  const refusal = guardRequest({ method: "POST", headers: { host: "127.0.0.1:5173" } });
  assert.equal(refusal?.status, 403);
  // Reads do not need it: the page loads provider state without a preflight.
  assert.equal(guardRequest({ method: "GET", headers: { host: "127.0.0.1:5173" } }), null);
});

test("non-http schemes and junk origins are refused", () => {
  assert.equal(isAllowedOrigin("file:///etc/passwd"), false);
  assert.equal(isAllowedOrigin("null"), false);
  assert.equal(isAllowedOrigin(""), false);
  assert.equal(isAllowedOrigin("data:text/html,x"), false);
});

test("loopback detection covers the whole /8 and rejects near-misses", () => {
  assert.equal(isLoopbackHost("127.0.0.1"), true);
  assert.equal(isLoopbackHost("127.13.9.4:5173"), true);
  assert.equal(isLoopbackHost("[::1]:5173"), true);
  assert.equal(isLoopbackHost("localhost:5173"), true);
  assert.equal(isLoopbackHost("127.0.0.1.evil.example"), false);
  assert.equal(isLoopbackHost("192.168.0.183"), false);
  assert.equal(isLoopbackHost("0.0.0.0"), false);
});
