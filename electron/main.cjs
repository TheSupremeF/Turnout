// Electron shell for the Turnout desktop app.
//
// CommonJS on purpose: Electron's ESM loader resolves the bare `electron`
// specifier to npm's wrapper package instead of the runtime builtin, so
// `import { app } from "electron"` fails with "does not provide an export
// named 'app'". Node has no such problem in CJS.
//
// The renderer is the same React GUI the browser build serves. It reaches the
// CLI through `/api/turnout/*` on a loopback origin, exactly as in development,
// so `src/App.jsx` needs no Electron-specific branch.
//
// Security posture: `contextIsolation` on, `nodeIntegration` off, `sandbox` on
// and no remote module, so a compromised page cannot spawn processes. Only this
// file spawns anything, and it only spawns `turnout`.

const { app, BrowserWindow, dialog, nativeImage, shell } = require("electron");
const { createServer } = require("node:http");
const { existsSync, readFileSync } = require("node:fs");
const { readFile } = require("node:fs/promises");
const path = require("node:path");

const here = __dirname;
const root = path.resolve(here, "..");

// `scripts/` is ESM (`type: module`), so it is loaded with dynamic `import()`
// rather than `require()`, which would throw ERR_REQUIRE_ESM.
let bridgeHandler = null;
let turnoutExecutable = null;
let turnoutLogoSvg = null;

async function loadModules() {
  const bridge = await import(path.join(root, "scripts", "turnout-bridge.mjs"));
  bridgeHandler = bridge.bridgeHandler;
  turnoutExecutable = bridge.turnoutExecutable;
  const logo = await import(path.join(root, "scripts", "logo.mjs"));
  turnoutLogoSvg = logo.turnoutLogoSvg;
}

let bridge = null;
let window = null;
let executable = null;

/// Where the packaged app puts the CLI: beside the executable inside
/// `resources/`, and in a source checkout inside `target/`.
function cliRoots() {
  return [process.resourcesPath, path.join(process.resourcesPath, "app"), root].filter(Boolean);
}

const CONTENT_TYPES = {
  ".html": "text/html; charset=utf-8",
  ".js": "text/javascript; charset=utf-8",
  ".css": "text/css; charset=utf-8",
  ".json": "application/json; charset=utf-8",
  ".svg": "image/svg+xml",
  ".woff2": "font/woff2",
  ".woff": "font/woff",
  ".png": "image/png",
  ".ico": "image/x-icon",
};

function contentType(file) {
  return CONTENT_TYPES[path.extname(file).toLowerCase()] || "application/octet-stream";
}

/// Serve the built GUI and the bridge from one ephemeral loopback port.
function startServer() {
  const clientDir = path.join(root, "dist", "client");
  const indexFile = path.join(clientDir, "index.html");

  return new Promise((resolve, reject) => {
    const server = createServer(async (request, response) => {
      try {
        if (await bridgeHandler(request, response, { executable })) return;

        // Static files. The renderer is a single page; unknown paths fall back
        // to index.html so client-side routes keep working.
        const url = new URL(request.url, "http://127.0.0.1");
        const requested = path.normalize(decodeURIComponent(url.pathname)).replace(/^(\.\.[/\\])+/, "");
        let file = path.join(clientDir, requested);
        if (!file.startsWith(clientDir) || !existsSync(file) || url.pathname === "/") {
          file = indexFile;
        }
        const body = await readFile(file);
        response.statusCode = 200;
        response.setHeader("Content-Type", contentType(file));
        response.setHeader("X-Content-Type-Options", "nosniff");
        // No remote code, no inline script, no eval. `data:` is needed for
        // fonts because Vite inlines small ones as data URIs.
        response.setHeader(
          "Content-Security-Policy",
          "default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; " +
            "font-src 'self' data:; img-src 'self' data:; connect-src 'self'; " +
            "object-src 'none'; base-uri 'none'; form-action 'none'",
        );
        response.end(body);
      } catch (error) {
        response.statusCode = 500;
        response.end(String((error && error.message) || error));
      }
    });

    server.on("error", reject);
    // Port 0: the OS picks a free port, so two copies or a busy 5173 never
    // collide. Loopback only, never all interfaces.
    server.listen(0, "127.0.0.1", () => {
      const { port } = server.address();
      resolve({ server, origin: `http://127.0.0.1:${port}` });
    });
  });
}

/// The dock icon is drawn from the shared symbol, so the mark has one source.
function setDockIcon() {
  if (process.platform !== "darwin" || !app.dock) return;
  try {
    const svg = turnoutLogoSvg({});
    const icon = nativeImage.createFromDataURL(
      `data:image/svg+xml;base64,${Buffer.from(svg).toString("base64")}`,
    );
    if (!icon.isEmpty()) app.dock.setIcon(icon);
  } catch {
    // A dock icon is cosmetic; never fail startup over it.
  }
}

function createWindow(origin) {
  window = new BrowserWindow({
    width: 1280,
    height: 840,
    minWidth: 940,
    minHeight: 620,
    backgroundColor: "#f4f5f8",
    title: "Turnout",
    icon: path.join(here, "icon.png"),
    show: false,
    webPreferences: {
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: true,
      preload: path.join(here, "preload.cjs"),
    },
  });

  window.once("ready-to-show", () => window.show());

  // The renderer only ever needs this origin. Anything else is a link the user
  // clicked and belongs in the real browser.
  window.webContents.setWindowOpenHandler(({ url }) => {
    if (/^https?:/.test(url)) shell.openExternal(url);
    return { action: "deny" };
  });
  window.webContents.on("will-navigate", (event, url) => {
    if (!url.startsWith(origin)) {
      event.preventDefault();
      if (/^https?:/.test(url)) shell.openExternal(url);
    }
  });

  window.loadURL(origin);
}

async function bootstrap() {
  try {
    await loadModules();
  } catch (error) {
    dialog.showErrorBox("Turnout could not start", String(error && error.message));
    app.quit();
    return;
  }
  setDockIcon();

  try {
    executable = await turnoutExecutable(cliRoots());
  } catch (error) {
    // Without the CLI the GUI cannot do anything; say so instead of showing a
    // permanently empty window.
    dialog.showErrorBox(
      "Turnout core not found",
      `${error.message}\n\nPlace the \`turnout\` executable next to the app or on your PATH, then restart.`,
    );
    app.quit();
    return;
  }

  const clientIndex = path.join(root, "dist", "client", "index.html");
  if (!existsSync(clientIndex)) {
    dialog.showErrorBox(
      "GUI is not built",
      "Run `npm run build` (or `npm run electron`, which builds first) and try again.",
    );
    app.quit();
    return;
  }

  const { server, origin } = await startServer();
  bridge = server;
  createWindow(origin);
}

app.whenReady().then(bootstrap);

app.on("window-all-closed", () => {
  if (bridge) bridge.close();
  if (process.platform !== "darwin") app.quit();
});

app.on("activate", () => {
  if (BrowserWindow.getAllWindows().length === 0 && bridge) bootstrap();
});

app.on("before-quit", () => {
  if (bridge) bridge.close();
});
