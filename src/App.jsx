import { createContext, useCallback, useContext, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { LANGUAGES, translate } from "./i18n";
import { TurnoutSymbol } from "../scripts/logo.mjs";
import {
  ArrowCounterClockwise,
  ArrowLeft,
  ArrowRight,
  CaretDown,
  Check,
  CheckCircle,
  CirclesThreePlus,
  Code,
  Command,
  File,
  FileText,
  GearSix,
  LinkSimple,
  LockKey,
  MagnifyingGlass,
  Play,
  PuzzlePiece,
  ShareNetwork,
  ShieldCheck,
  SlidersHorizontal,
  Stack,
  TextAlignLeft,
  Info,
  Warning,
  WarningCircle,
  SunDim,
  TerminalWindow,
  X,
} from "@phosphor-icons/react";

const LANGUAGE_STORAGE_KEY = "turnout.language.v1";
const LanguageContext = createContext("tr");
const LanguageUpdateContext = createContext(() => {});

// Every visible string goes through `t`. The key is the English text, so an
// untranslated key is still readable instead of showing a raw identifier.
function useT() {
  const language = useContext(LanguageContext);
  return useCallback((key) => translate(language, key), [language]);
}

function useLanguage() {
  return useContext(LanguageContext);
}
function useSetLanguage() {
  return useContext(LanguageUpdateContext);
}

const EVREN_MODELS = [
  "auto",
  "deepseek-v4.1-flash",
  "deepseek-v4-flash",
  "mimo-v2.6-pro",
  "qwen3.8-flash-next",
  "glm-5.3",
  "gemma-4-31b",
  "qwen3-vl-30b",
  "qwen3-guard-4b",
  "laya",
  "clef",
  "deepseek-ocr-2",
  "dots-ocr",
  "qwen3-embedding-8b",
  "qwen3-reranker-8b",
  "qwen3-asr-1.7b",
  "voxcpm2",
];

const OPENAI_MODELS = [
  { model: "gpt-6.1-sol", label: "GPT-6.1 Sol" },
  { model: "gpt-6-astra", label: "GPT-6 Astra" },
  { model: "gpt-6-sol", label: "GPT-6 Sol" },
  { model: "gpt-6-luna", label: "GPT-6 Luna" },
  { model: "gpt-5.6-sol", label: "GPT-5.6 Sol" },
  { model: "gpt-5.6-terra", label: "GPT-5.6 Terra" },
  { model: "gpt-5.6-luna", label: "GPT-5.6 Luna" },
];

// Claude Code 2.1.153 binary'sinde tanımlı model kimlikleri (tarih ekleri ve
// -v1 varyantları çıkarıldı). Liste binary'den gelir, uydurulmaz.
const ANTHROPIC_MODELS = [
  { model: "claude-opus-4-7", label: "Claude Opus 4.7" },
  { model: "claude-opus-4-6", label: "Claude Opus 4.6" },
  { model: "claude-opus-4-6-fast", label: "Claude Opus 4.6 Fast" },
  { model: "claude-opus-4-5", label: "Claude Opus 4.5" },
  { model: "claude-opus-4-1", label: "Claude Opus 4.1" },
  { model: "claude-opus-4", label: "Claude Opus 4" },
  { model: "claude-sonnet-4-6", label: "Claude Sonnet 4.6" },
  { model: "claude-sonnet-4-5", label: "Claude Sonnet 4.5" },
  { model: "claude-sonnet-4", label: "Claude Sonnet 4" },
  { model: "claude-sonnet-3-7", label: "Claude Sonnet 3.7" },
  { model: "claude-haiku-4-5", label: "Claude Haiku 4.5" },
  { model: "claude-haiku-4", label: "Claude Haiku 4" },
  { model: "claude-haiku-3-5", label: "Claude Haiku 3.5" },
];

// Claude 5 ailesi. Etiket kullanıcıya gösterilir, `model` ise upstream'e giden
// gerçek API kimliğidir; "Opus 5.5" gibi görünen adlar API'de geçersizdir.
const CUSTOM_ANTHROPIC_MODELS = [
  { model: "claude-fable-5-1", label: "Fable 5.1" },
  { model: "claude-opus-5-5", label: "Opus 5.5" },
  { model: "claude-sonnet-5-5", label: "Sonnet 5.5" },
  { model: "claude-haiku-5-5", label: "Haiku 5.5" },
];

// Eski sürümlerin localStorage'a yazdığı görünen adlar -> gerçek kimlikler.
const LEGACY_MODEL_IDS = {
  "Fable 5.1": "claude-fable-5-1",
  "Opus 5.5": "claude-opus-5-5",
  "Sonnet 5.5": "claude-sonnet-5-5",
  "Haiku 5.5": "claude-haiku-5-5",
};
const migrateModel = (model) => LEGACY_MODEL_IDS[model] ?? model;

const PROVIDERS = {
  // `catalog` is how many models Turnout knows for the provider, not how many
  // any route uses; it is always shown with the word "Catalog".
  EVREN: { id: "evren", name: "EVREN", icon: ShareNetwork, dot: "mint", catalog: EVREN_MODELS.length },
  Anthropic: {
    id: "anthropic",
    name: "Anthropic",
    icon: SunDim,
    dot: "blue",
    catalog: ANTHROPIC_MODELS.length + CUSTOM_ANTHROPIC_MODELS.length,
  },
  OpenAI: { id: "openai", name: "OpenAI", icon: CirclesThreePlus, dot: "violet", catalog: OPENAI_MODELS.length },
};

const providerId = (name) => PROVIDERS[name]?.id ?? String(name).toLowerCase();
const providerConnection = (connections, name) => connections.find(
  (connection) => connection.id === providerId(name),
);
const oauthConnection = (connections, name) => connections.find(
  (connection) => connection.id === providerId(name),
);
const targetReady = (route, target, connections, oauthConnections) => {
  if (providerConnection(connections, target.provider)?.has_key) return true;
  if (route.id === "codex" && providerId(target.provider) === "openai") {
    return Boolean(oauthConnection(oauthConnections, "OpenAI")?.logged_in);
  }
  if (route.id === "claude" && providerId(target.provider) === "anthropic") {
    return Boolean(oauthConnection(oauthConnections, "Anthropic")?.logged_in);
  }
  return false;
};

const MODEL_OPTIONS = [
  ...OPENAI_MODELS.map((option) => ({ provider: "OpenAI", ...option })),
  ...EVREN_MODELS.map((model) => ({ provider: "EVREN", model })),
  ...[...CUSTOM_ANTHROPIC_MODELS, ...ANTHROPIC_MODELS].map((option) => ({
    provider: "Anthropic",
    ...option,
  })),
];

const modelLabel = (provider, model) => MODEL_OPTIONS.find(
  (option) => option.provider === provider && option.model === model,
)?.label ?? model;

// Claude Code and Codex can keep their built-in model list and append models
// from other providers after it. Claude's list stays on Claude's own login;
// Codex's is served by the OpenAI connection under the original ids.
const ORIGINAL_LIST_PROVIDER = { claude: "anthropic", codex: "openai" };
const canKeepOriginalList = (route) => Boolean(ORIGINAL_LIST_PROVIDER[route.id])
  && route.targets.some((target) => providerId(target.provider) !== ORIGINAL_LIST_PROVIDER[route.id]);
const ORIGINAL_LIST_COPY = {
  claude: {
    title: "Keep Claude's own models",
    detail: "Official Claude models stay on your own account; the custom models you select are appended.",
  },
  codex: {
    title: "Keep Codex's own models",
    detail: "Codex's original list (GPT models) stays on your ChatGPT session; the custom models you select are appended.",
  },
};

const ROUTES_STORAGE_KEY = "turnout.routes.v1";
const TESTS_STORAGE_KEY = "turnout.tests.v1";

// The Routing screen edits the CLI launch profile `gui-<route>`; the gateway
// route the VS Code extension uses is shown beside it, read-only. Both come
// from `turnout profiles --json`, so no number on screen is a local guess.
const CLI_HARNESS = { codex: "codex", opencode: "opencode", claude: "claude-code" };
const GUI_PROVIDER = { openai: "OpenAI", evren: "EVREN", anthropic: "Anthropic" };

function routeFromCli(route) {
  if (!route?.targets?.length) return null;
  const targets = route.targets.map((target) => ({
    provider: GUI_PROVIDER[target.provider] ?? target.provider,
    model: target.model,
  }));
  // `model` is the public id (`openai/gpt-6.1-sol` in profiles, bare in routes).
  const index = route.targets.findIndex((target) => target.public_model === route.model
    || target.model === route.model
    || `${target.provider}/${target.model}` === route.model);
  const defaultTarget = targets[Math.max(0, index)];
  // The CLI omits `fallback` while it is on (home.rs: default_true, skip if true).
  return { targets, provider: defaultTarget.provider, model: defaultTarget.model, fallback: route.fallback !== false };
}

function routingFromProfiles(result) {
  const byRoute = {};
  for (const [id, harness] of Object.entries(CLI_HARNESS)) {
    const entry = result.harnesses?.find((item) => item.harness === harness);
    const profile = entry?.profiles?.find((item) => item.name === `gui-${id}`);
    const profileRoute = profile ? routeFromCli(profile.route) : null;
    const applied = routeFromCli(entry?.applied);
    byRoute[id] = {
      saved: profileRoute
        ? { ...profileRoute, source: "profile", profile: profile.name }
        : applied ? { ...applied, source: "applied" } : null,
      applied,
    };
  }
  return byRoute;
}

const routeTargetKey = (target) => `${target.provider}::${target.model}`;

function pendingChanges(route, saved) {
  if (!saved) return 0;
  const local = new Set(route.targets.map(routeTargetKey));
  const stored = new Set(saved.targets.map(routeTargetKey));
  let changes = 0;
  for (const key of local) if (!stored.has(key)) changes += 1;
  for (const key of stored) if (!local.has(key)) changes += 1;
  if (route.provider !== saved.provider || route.model !== saved.model) changes += 1;
  return changes;
}

function initialRoutes() {
  try {
    const saved = JSON.parse(window.localStorage.getItem(ROUTES_STORAGE_KEY) ?? "null");
    if (!Array.isArray(saved)) return INITIAL_ROUTES;
    return INITIAL_ROUTES.map((route) => {
      const match = saved.find((entry) => entry.id === route.id);
      if (!match) return route;
      // Eski kayıtlardaki `lastTest`/`latency` mock'tan kalmaydı; gerçek test
      // zamanı artık `lastTestAt` alanında tutuluyor.
      const { lastTest: _fakeTime, latency: _fakeLatency, ...rest } = match;
      return {
        ...route,
        ...rest,
        model: migrateModel(rest.model ?? route.model),
        targets: (rest.targets ?? route.targets).map((target) => ({ ...target, model: migrateModel(target.model) })),
        Icon: route.Icon,
      };
    });
  } catch {
    return INITIAL_ROUTES;
  }
}

const INITIAL_ROUTES = [
  {
    id: "codex",
    name: "Codex",
    subtitle: "OpenAI coding tool",
    Icon: TerminalWindow,
    provider: "OpenAI",
    model: "gpt-6.1-sol",
    targets: OPENAI_MODELS.map(({ model }) => ({ provider: "OpenAI", model })),
    files: ["~/.codex/config.toml"],
    keepOriginal: true,
    fallback: true,
  },
  {
    id: "opencode",
    name: "OpenCode",
    subtitle: "Open-source coding tool",
    Icon: Code,
    provider: "EVREN",
    model: "mimo-v2.6-pro",
    targets: ["mimo-v2.6-pro", "deepseek-v4.1-flash"].map((model) => ({ provider: "EVREN", model })),
    files: ["~/.config/opencode/opencode.json"],
    fallback: true,
  },
  {
    id: "claude",
    name: "Claude Code",
    subtitle: "Anthropic coding tool",
    Icon: TextAlignLeft,
    provider: "EVREN",
    model: "deepseek-v4.1-flash",
    targets: ["deepseek-v4.1-flash", "mimo-v2.6-pro"].map((model) => ({ provider: "EVREN", model })),
    files: ["~/.claude/settings.json"],
    keepOriginal: true,
    fallback: true,
  },
];

const NAV_ITEMS = [
  { id: "connections", label: "Connections", Icon: LinkSimple },
  { id: "routing", label: "Routing", Icon: ShareNetwork },
  { id: "extensions", label: "VS Code Extensions", Icon: PuzzlePiece },
  { id: "profiles", label: "Profiles", Icon: Stack },
  { id: "logs", label: "Logs", Icon: FileText },
  { id: "settings", label: "Settings", Icon: GearSix },
];

const PAGE_COPY = {
  connections: [
    "Connections",
    "Add providers. Keys stay in a file only you can read; they are never written to config files.",
  ],
  routing: [
    "Routing",
    "Connect your providers to Codex, OpenCode and Claude Code. Config files are backed up.",
  ],
  extensions: [
    "VS Code Extensions",
    "Inject the route you selected into the editor extension, independently of CLI profiles.",
  ],
  profiles: [
    "Profiles",
    "Routing sets for different working environments.",
  ],
  logs: [
    "Logs",
    "Review connection tests and configuration changes.",
  ],
  settings: [
    "Settings",
    "Manage backups, security and application preferences.",
  ],
};

const PROVIDER_LETTER = { evren: "E", openai: "O", anthropic: "A" };

function TopBar({ onSearch, page, agentRunning }) {
  const t = useT();
  const [title, subtitle] = PAGE_COPY[page] ?? ["", ""];
  return (
    <header className="topbar">
      <div className="page-heading">
        <h1>
          {t(title)}
          {page === "routing" && (
            <span className={`status-pill agent ${agentRunning ? "" : "offline"}`}>
              <span className={`dot ${agentRunning ? "" : "offline"}`} />
              {agentRunning ? t("Agent running") : t("Agent stopped")}
            </span>
          )}
        </h1>
        <p>{t(subtitle)}</p>
      </div>
      <button className="search-trigger" onClick={onSearch} type="button" aria-label={t("Search or run a command")}>
        <MagnifyingGlass size={16} />
        <span className="search-text">{t("Search or run a command")}</span>
        <kbd>⌘K</kbd>
      </button>
    </header>
  );
}

function TurnoutLogo({ size = 78 }) {
  // The mark lives in `scripts/logo.mjs` so the GUI, the app icon and the
  // README cannot drift apart.
  return (
    <span
      className="turnout-logo"
      style={{ width: size, height: size, display: "block" }}
      aria-hidden="true"
      dangerouslySetInnerHTML={{ __html: TurnoutSymbol({ size }) }}
    />
  );
}

function Sidebar({ active, onChange }) {
  const t = useT();
  const renderItem = ({ id, label, Icon }) => (
    <button
      key={id}
      type="button"
      className={`rail-btn ${active === id ? "active" : ""}`}
      onClick={() => onChange(id)}
      aria-label={t(label)}
      aria-current={active === id ? "page" : undefined}
      title={t(label)}
    >
      <Icon size={21} />
    </button>
  );
  return (
    <aside className="rail" aria-label={t("Main menu")}>
      <div className="rail-logo">
        <TurnoutLogo />
      </div>
      {NAV_ITEMS.filter((item) => item.id !== "settings").map(renderItem)}
      <div className="rail-spacer" />
      {NAV_ITEMS.filter((item) => item.id === "settings").map(renderItem)}
    </aside>
  );
}

function ProvidersBar({ connections, oauthConnections, loading }) {
  const t = useT();
  return (
    <section className="providers-bar" aria-label={t("Providers")}>
      {Object.values(PROVIDERS).map((provider) => {
        const Icon = provider.icon;
        const connection = providerConnection(connections, provider.name);
        const oauth = oauthConnection(oauthConnections, provider.name);
        const connected = Boolean(connection?.has_key || oauth?.logged_in);
        // A Claude subscription session only works inside Claude Code; it is
        // not a credential Turnout can hand to Codex, OpenCode or the gateway.
        const subscriptionOnly = provider.id === "anthropic" && !connection?.has_key && Boolean(oauth?.logged_in);
        const dot = loading ? "offline" : subscriptionOnly ? "amber" : (connected ? "" : "offline");
        const catalog = `${t("Catalog")} ${provider.catalog}`;
        // While connections are still being read, an empty list is not "no key".
        const detail = loading ? `${catalog} · ${t("Reading…")}` : subscriptionOnly
          ? `Subscription · ${oauth.plan || "Claude"}`
          : connection?.has_key
            ? `${catalog} · ${t("Key ready")}`
            : connected
              ? `${catalog} · OAuth · ${oauth.plan || "account"}`
              : `${catalog} · ${t("No key")}`;
        return (
          <article className="provider-item" key={provider.name}>
            <span className={`provider-icon ${provider.id}`} aria-hidden="true"><Icon size={18} /></span>
            <div className="provider-copy">
              <div className="provider-name">{provider.name}<span className={`dot ${dot}`} /></div>
              <div className="provider-count">{detail}</div>
            </div>
            <span className={`status-pill ${loading ? "offline" : subscriptionOnly ? "limited" : (connected ? "" : "offline")}`}>
              {loading ? t("Reading…") : subscriptionOnly ? t("Claude Code only") : (connected ? t("Connected") : t("Not connected"))}
            </span>
          </article>
        );
      })}
    </section>
  );
}

function ModelSelect({ route, onChange, connections, oauthConnections, routingStatus, pending }) {
  const t = useT();
  const [catalogProvider, setCatalogProvider] = useState(route.provider);
  const providerModels = MODEL_OPTIONS.filter((option) => option.provider === catalogProvider);
  // Until the CLI state is read the route on screen is only a placeholder;
  // opening the picker would mean editing a guess.
  const ready = routingStatus === "ready";
  // Routed models the catalog does not know (e.g. `gpt-5.5`) must stay visible,
  // otherwise the picker hides part of the real route.
  const offCatalog = route.targets.filter((target) => target.provider === catalogProvider
    && !providerModels.some((option) => option.model === target.model));
  const defaultLabel = modelLabel(route.provider, route.model);
  const targetKey = (provider, model) => `${provider}::${model}`;
  const selectedKeys = new Set(route.targets.map((target) => targetKey(target.provider, target.model)));

  const toggleModel = (provider, model) => {
    const key = targetKey(provider, model);
    const selected = selectedKeys.has(key);
    if (selected && route.targets.length === 1) return;
    const targets = selected
      ? route.targets.filter((target) => targetKey(target.provider, target.model) !== key)
      : [...route.targets, { provider, model }];
    const defaultStillSelected = targets.some(
      (target) => target.provider === route.provider && target.model === route.model,
    );
    const nextDefault = defaultStillSelected ? { provider: route.provider, model: route.model } : targets[0];
    onChange({ targets, ...nextDefault });
  };

  const detailsRef = useRef(null);
  const menuRef = useRef(null);
  const [open, setOpen] = useState(false);
  const [menuStyle, setMenuStyle] = useState(null);

  // The menu is position:fixed so the matrix panel / scroll area cannot clip it.
  useLayoutEffect(() => {
    if (!open) return undefined;
    const place = () => {
      const trigger = detailsRef.current?.querySelector("summary");
      const menu = menuRef.current;
      if (!trigger || !menu) return;
      const rect = trigger.getBoundingClientRect();
      const gap = 6;
      const margin = 12;
      const width = Math.min(340, window.innerWidth - margin * 2);
      const height = menu.offsetHeight;
      const left = Math.max(margin, Math.min(rect.right - width, window.innerWidth - width - margin));
      const below = window.innerHeight - rect.bottom - gap - margin;
      const above = rect.top - gap - margin;
      const top = height <= below || below >= above
        ? rect.bottom + gap
        : Math.max(margin, rect.top - gap - height);
      setMenuStyle({ top, left, width, maxHeight: Math.max(below, above) });
    };
    place();
    const close = (event) => {
      if (event.type === "keydown" && event.key !== "Escape") return;
      if (event.type === "pointerdown" && detailsRef.current?.contains(event.target)) return;
      setOpen(false);
    };
    window.addEventListener("resize", place);
    window.addEventListener("scroll", place, true);
    document.addEventListener("pointerdown", close);
    document.addEventListener("keydown", close);
    return () => {
      window.removeEventListener("resize", place);
      window.removeEventListener("scroll", place, true);
      document.removeEventListener("pointerdown", close);
      document.removeEventListener("keydown", close);
    };
  }, [open, catalogProvider]);

  return (
    <details
      ref={detailsRef}
      className="model-select"
      open={open}
      onToggle={(event) => setOpen(ready && event.currentTarget.open)}
      onClick={(event) => event.stopPropagation()}
    >
      <summary
        className="model-select-trigger"
        aria-disabled={ready ? undefined : "true"}
        onClick={(event) => { if (!ready) event.preventDefault(); }}
      >
        <span className={`model-mark ${providerId(route.provider)}`} aria-hidden="true">{ready ? (PROVIDER_LETTER[providerId(route.provider)] ?? "?") : "·"}</span>
        <span className="model-copy">
          {ready ? (
            <>
              <strong>{defaultLabel}</strong>
              <small>
                {route.provider} · {route.targets.length} model{canKeepOriginalList(route) && route.keepOriginal !== false ? ` · ${t("original list kept")}` : ""}
                {pending > 0 && <> · <span className="pending-text">{pending} {t(pending === 1 ? "unsaved change" : "unsaved changes")}</span></>}
              </small>
            </>
          ) : (
            <>
              <strong className="unknown-text">{routingStatus === "loading" ? t("Reading…") : t("unknown")}</strong>
              <small className="unknown-text">{routingStatus === "loading" ? t("Reading…") : t("unknown")}</small>
            </>
          )}
        </span>
        <CaretDown size={13} weight="bold" aria-hidden="true" />
      </summary>
      <div className="model-menu" ref={menuRef} style={menuStyle ?? { visibility: "hidden" }}>
        <label className="model-provider-choice">
          <span>{t("Provider")}</span>
          <select value={catalogProvider} onChange={(event) => setCatalogProvider(event.target.value)}>
            {Object.keys(PROVIDERS).map((provider) => {
              const routed = route.targets.filter((target) => target.provider === provider).length;
              const connected = targetReady(route, { provider }, connections, oauthConnections);
              const counts = `${t("Catalog")} ${PROVIDERS[provider].catalog} · ${t("routed")} ${routed}`;
              return <option key={provider} value={provider}>{provider} · {counts}{connected ? "" : (provider === "Anthropic" && route.id !== "claude" ? ` · ${t("API key required")}` : ` · ${t("connection required")}`)}</option>;
            })}
          </select>
        </label>
        {catalogProvider === "OpenAI" && (
          <button
            className="recommended-models"
            type="button"
            onClick={() => {
              const targets = [...route.targets];
              for (const { model } of OPENAI_MODELS) {
                if (!targets.some((target) => target.provider === "OpenAI" && target.model === model)) {
                  targets.push({ provider: "OpenAI", model });
                }
              }
              onChange({ provider: "OpenAI", model: "gpt-6.1-sol", targets });
            }}
          >
            <strong>{t("Default")}</strong>
            <span>{t("Recommended set of models")}</span>
          </button>
        )}
        <div className="model-menu-heading"><span>{t("Catalog models")}</span><span>{t("Default")}</span></div>
        <div className="model-menu-list">
          {providerModels.map(({ model, label }) => {
            const selected = selectedKeys.has(targetKey(catalogProvider, model));
            return (
              <div className={`model-option ${selected ? "selected" : ""}`} key={model}>
                <label>
                  <input type="checkbox" checked={selected} onChange={() => toggleModel(catalogProvider, model)} />
                  <span>{label ?? model}</span>
                </label>
                <input
                  aria-label={`${model} ${t("Default")}`}
                  type="radio"
                  name={`${route.id}-default-model`}
                  checked={route.provider === catalogProvider && route.model === model}
                  onChange={() => onChange({
                    provider: catalogProvider,
                    model,
                    targets: selected ? route.targets : [...route.targets, { provider: catalogProvider, model }],
                  })}
                />
              </div>
            );
          })}
          {offCatalog.length > 0 && (
            <>
              <div className="model-menu-section">{t("Routed but not in the catalog")} · {offCatalog.length}</div>
              {offCatalog.map(({ model }) => (
                <div className="model-option selected off-catalog" key={`off-${model}`}>
                  <label>
                    <input type="checkbox" checked onChange={() => toggleModel(catalogProvider, model)} />
                    <span>{model}</span>
                  </label>
                  <input
                    aria-label={`${model} ${t("Default")}`}
                    type="radio"
                    name={`${route.id}-default-model`}
                    checked={route.provider === catalogProvider && route.model === model}
                    onChange={() => onChange({ provider: catalogProvider, model, targets: route.targets })}
                  />
                </div>
              ))}
            </>
          )}
        </div>
        <p>{t("Changing the provider keeps earlier selections. Every selected model appears in the harness menu.")}</p>
        {pending > 0 && (
          <p role="status" className="pending-text">{pending} {t(pending === 1 ? "unsaved change" : "unsaved changes")}. {t("The saved profile is unchanged until you save.")}</p>
        )}
      </div>
    </details>
  );
}

function RoutingTable({ routes, selectedId, onSelect, onModelChange, connections, oauthConnections, connectionsLoading, routingStatus, pendingById }) {
  const t = useT();
  const language = useLanguage();
  const timeFormat = language === "tr" ? "tr-TR" : undefined;
  return (
    <div>
      <div className="matrix-head">
        <span>{t("Harness")}</span>
        <span>{t("Model")}</span>
        <span>{t("Status")}</span>
      </div>
      {routes.map((route) => {
        const Icon = route.Icon;
        const ready = route.targets.every((target) => targetReady(route, target, connections, oauthConnections));
        return (
          <div
            key={route.id}
            className={`matrix-row ${selectedId === route.id ? "selected" : ""}`}
            onClick={() => onSelect(route.id)}
          >
            <button className="harness-cell" type="button" onClick={() => onSelect(route.id)}>
              <span className="harness-icon"><Icon size={17} /></span>
              <span><strong className="harness-name">{route.name}</strong><small className="harness-sub">{t(route.subtitle)}</small></span>
            </button>
            <ModelSelect route={route} connections={connections} oauthConnections={oauthConnections} routingStatus={routingStatus} pending={pendingById[route.id] ?? 0} onChange={(patch) => onModelChange(route.id, patch)} />
            {connectionsLoading ? (
              <div className="route-status">
                <strong className="unknown-text"><span className="dot offline" />{t("Reading…")}</strong>
              </div>
            ) : (
            <div className="route-status">
              <strong className={ready ? "" : "offline-text"}><span className={`dot ${ready ? "" : "amber"}`} />{ready ? t("Ready") : t("Key missing")}</strong>
              <small>{ready ? (route.lastTestAt ? `${t("Last test")}: ${new Date(route.lastTestAt).toLocaleTimeString(timeFormat, { hour: "2-digit", minute: "2-digit" })}` : t("Not tested yet")) : t("Complete the connections")}</small>
            </div>
            )}
          </div>
        );
      })}
    </div>
  );
}

// Fallback chain as the gateway builds it (relay-cli main.rs: requested model
// first, then the route's other models in route order). Shown for the default
// request; long chains collapse to the first three.
function FallbackChain({ route }) {
  const t = useT();
  const [expanded, setExpanded] = useState(false);
  const first = route.targets.find((target) => target.provider === route.provider && target.model === route.model)
    ?? route.targets[0];
  if (!first) return null;
  const chain = route.fallback === false
    ? [first]
    : [first, ...route.targets.filter((target) => routeTargetKey(target) !== routeTargetKey(first))];
  const shown = expanded ? chain : chain.slice(0, 3);
  const hidden = chain.length - shown.length;
  return (
    <section className="chain-block" aria-labelledby={`chain-${route.id}`}>
      <div className="chain-head">
        <h3 id={`chain-${route.id}`}>{t("Fallback order")}</h3>
        <span>{t("read-only")}</span>
      </div>
      <ol className="chain">
        {shown.map((target, index) => (
          <li key={routeTargetKey(target)} className={index === 0 ? "first" : ""}>
            <span className="chain-n">{index + 1}</span>
            <span>{target.model}</span>
            {index === 0 && <span className="chain-default">{t("default")}</span>}
          </li>
        ))}
      </ol>
      {chain.length > 3 && (
        <button className="chain-toggle" type="button" aria-expanded={expanded} onClick={() => setExpanded((value) => !value)}>
          {expanded ? t("Show fewer") : `${hidden} ${t("more models")}`}
        </button>
      )}
      <p className="chain-note">
        {route.fallback === false
          ? t("Fallback is off for this route; only the requested model is used.")
          : t("Chain for a default request. The requested model is tried first; on overload, rate limit or an unreachable provider the route's other models are tried in route order. Once a response starts streaming it is not retried.")}
      </p>
    </section>
  );
}

function Inspector({ route, testState, applyState, onTest, onApply, connections, oauthConnections, connectionsLoading, routing, routingStatus, pending, testRecord }) {
  const t = useT();
  const language = useLanguage();
  const timeFormat = language === "tr" ? "tr-TR" : undefined;
  const Icon = route.Icon;
  const ready = routingStatus === "ready";
  const saved = routing?.saved ?? null;
  const applied = routing?.applied ?? null;
  const testing = testState === "testing";
  const [elapsed, setElapsed] = useState(0);
  useEffect(() => {
    if (!testing) return undefined;
    const startedAt = Date.now();
    setElapsed(0);
    const timer = window.setInterval(() => setElapsed(Math.floor((Date.now() - startedAt) / 1000)), 1000);
    return () => window.clearInterval(timer);
  }, [testing]);
  const formatTime = (iso) => new Date(iso).toLocaleString(timeFormat, { day: "2-digit", month: "2-digit", hour: "2-digit", minute: "2-digit" });
  const connected = targetReady(
    route,
    { provider: route.provider, model: route.model },
    connections,
    oauthConnections,
  );
  const nativeOAuth = (route.id === "codex" && providerId(route.provider) === "openai")
    || (route.id === "claude" && providerId(route.provider) === "anthropic");
  const needsAnthropicKey = !connected && providerId(route.provider) === "anthropic" && route.id !== "claude";
  const sourceLabel = !ready
    ? <span className="unknown-text">{routingStatus === "loading" ? t("Reading…") : t("unknown")}</span>
    : saved?.source === "profile"
      ? <span className="source-caption">{t("CLI profile")} <code>{saved.profile}</code></span>
      : saved ? t("gateway route (no profile yet)") : t("No route yet");
  return (
    <aside className="inspector">
      <div className="inspector-head-row">
        <h2>{t("Selected route")}</h2>
        <span className="status-pill soft">{ready ? `${route.targets.length} ${t("models")}` : "—"}</span>
      </div>

      <div className="selected-harness">
        <span className="selected-icon"><Icon size={17} /></span>
        <span><strong>{route.name}</strong><small>{t(route.subtitle)}</small></span>
      </div>

      <dl className="route-details">
        <div><dt>{t("Provider")}</dt><dd>{route.provider}{connectionsLoading
          ? <span className="status-pill offline">{t("Reading…")}</span>
          : <span className={`status-pill ${connected ? "" : "offline"}`}>{connected ? (nativeOAuth ? "OAuth" : t("Connected")) : (needsAnthropicKey ? t("API key required") : t("Not connected"))}</span>}</dd></div>
        <div><dt>{t("Default")}</dt><dd className="inspector-model">{ready ? route.model : <span className="unknown-text">—</span>}</dd></div>
        <div><dt>{t("Source")}</dt><dd>{sourceLabel}</dd></div>
        {ready && applied && (
          <div><dt>{t("VS Code / gateway")}</dt><dd>{applied.targets.length} {t("models")} · <span className="inspector-model">{applied.model}</span></dd></div>
        )}
      </dl>

      {ready && pending > 0 && (
        <div className="pending-note"><strong>{pending} {t(pending === 1 ? "unsaved change" : "unsaved changes")}.</strong> {t("The saved profile is unchanged until you save.")}</div>
      )}

      {ready && (saved || route).targets.length > 0 && <FallbackChain key={route.id} route={saved ?? route} />}

      <button className="test-button" type="button" onClick={onTest} disabled={testing || !ready}>
        {testing ? <span className="spinner" aria-hidden="true" /> : <Play size={14} weight="fill" />}
        {testing ? `${t("Testing…")} ${elapsed}s` : t("Test connection")}
      </button>

      {/* Mounted from first paint so assistive tech is already listening when a result arrives. */}
      <div role="status" aria-live="polite">
        {testing ? (
          <div className="test-result show testing">
            <Info size={20} />
            <div><strong>{t("Connection test is running")}</strong><span>{t("Model")}: {route.model}</span></div>
          </div>
        ) : testRecord ? (
          <div className={`test-result show ${testRecord.ok ? "" : "error"}`}>
            {testRecord.ok ? <CheckCircle size={20} weight="fill" /> : <WarningCircle size={20} weight="fill" />}
            <div>
              <strong>{testRecord.ok ? t("Connection successful") : t("Connection failed")}</strong>
              <span>{t("Model")}: {testRecord.model}</span>
              {testRecord.detail && <span>{testRecord.detail}</span>}
              {!testRecord.ok && <span>{t("What to do: check the provider key on the Connections page, then test again.")}</span>}
              <span className="when">{formatTime(testRecord.at)} · {t("saved result")}</span>
            </div>
          </div>
        ) : (
          <div className="test-note">
            <Info size={17} />
            <div>{route.lastTestAt
              ? `${t("Last test")}: ${new Date(route.lastTestAt).toLocaleTimeString(timeFormat, { hour: "2-digit", minute: "2-digit" })}. ${t("The test measures latency with a real request.")}`
              : t("This route has not been tested yet. The test measures latency with a real request.")}</div>
          </div>
        )}
      </div>
      {!ready && routingStatus === "error" && <p className="chain-note">{t("Testing and saving are off until the routing state is read.")}</p>}

      <div className="files-block">
        <h3>{t("Files to change")}</h3>
        <ul>
          {route.files.map((file) => <li key={file}><File size={14} /><span>{file}</span></li>)}
        </ul>
      </div>

      <div className="profile-box cli-profile-note">
        <strong><LockKey size={14} />{t("CLI profile")}</strong>
        <small>{t("The config file is not changed. The route is injected only into the process started with Turnout.")}</small>
        <code>turnout run {route.id} --profile gui-{route.id}</code>
      </div>

      <button className="apply-button" type="button" onClick={onApply} disabled={applyState === "applying" || !ready}>
        {applyState === "applying" ? <span className="spinner light" /> : <Check size={16} weight="bold" />}
        {applyState === "applying" ? t("Saving") : t("Save CLI profile")}
      </button>

      <p className="security-note">
        <ShieldCheck size={13} />
        <span>{t("Keys are kept in")} <span className="mono">credentials.json</span> {t("with user-only access.")}</span>
      </p>
    </aside>
  );
}

const EXTENSION_META = {
  codex: { extension: "openai.chatgpt", detail: "Codex VS Code eklentisi", config: "~/.codex/config.toml" },
  opencode: { extension: "sst-dev.opencode", detail: "OpenCode VS Code eklentisi", config: "~/.config/opencode/opencode.json" },
  claude: { extension: "anthropic.claude-code", detail: "Claude Code VS Code eklentisi", config: "~/.claude/settings.json" },
};

function ExtensionsPage({ routes, connections, oauthConnections, applyState, onInject, onRouteChange }) {
  const t = useT();
  return (
    <main className="content extensions-page">
      <section className="extension-intro">
        <Warning size={20} />
        <div><b>{t("A separate, explicit action.")}</b> {t("The buttons on this screen change the harness's shared config file with a backup and prepare the Turnout Agent. The CLI profile on the Routing screen does not touch these files.")}</div>
      </section>
      <div className="extension-grid">
        {routes.map((route) => {
          const Icon = route.Icon;
          const meta = EXTENSION_META[route.id];
          const ready = route.targets.every((target) => targetReady(route, target, connections, oauthConnections));
          const applying = applyState === `vscode:${route.id}`;
          // The option is always offered where the harness has its own list;
          // it only takes effect once a model from another provider is selected.
          const keepOption = Boolean(ORIGINAL_LIST_PROVIDER[route.id]);
          const keepUsable = canKeepOriginalList(route);
          return (
            <article className="extension-card" key={route.id}>
              <div className="extension-card-head">
                <span className="selected-icon"><Icon size={18} /></span>
                <div><strong>{route.name}</strong><span>{meta.detail}</span><code>{meta.extension}</code></div>
                <span className={`status-pill ${ready ? "" : "limited"}`}><span className={`dot ${ready ? "" : "amber"}`} />{ready ? t("Ready") : t("Connections incomplete")}</span>
              </div>
              <dl className="extension-dl">
                <div><dt>{t("Default")}</dt><dd className="mono">{route.model}</dd></div>
                <div><dt>{t("Models")}</dt><dd>{route.targets.length}</dd></div>
                <div><dt>{t("Config")}</dt><dd className="mono">{meta.config}</dd></div>
              </dl>
              {!keepOption && route.targets.length <= 4 && (
                <div className="extension-models">
                  {route.targets.map((target) => <span className="model-chip" key={`${target.provider}::${target.model}`}>{target.model}</span>)}
                </div>
              )}
              {keepOption && (
                <label className={`extension-option${keepUsable ? "" : " is-disabled"}`}>
                  <input type="checkbox" disabled={!keepUsable} checked={keepUsable && route.keepOriginal !== false} onChange={(event) => onRouteChange(route.id, { keepOriginal: event.target.checked })} />
                  <span>
                    <b>{t(ORIGINAL_LIST_COPY[route.id].title)}</b>
                    <small>
                      {keepUsable
                        ? t(ORIGINAL_LIST_COPY[route.id].detail)
                        : `${t("To append to the original list, select at least one model from outside")} ${ORIGINAL_LIST_PROVIDER[route.id] === "openai" ? "OpenAI" : "Anthropic"} ${t("on the Routing screen (for example EVREN).")}`}
                    </small>
                  </span>
                </label>
              )}
              <label className="extension-option">
                <input type="checkbox" checked={route.fallback !== false} onChange={(event) => onRouteChange(route.id, { fallback: event.target.checked })} />
                <span>
                  <b>{t("Fall back to another model")}</b>
                  <small>{t("If the selected provider is unreachable or rate-limited, the gateway retries the same request with the route's other models. Streaming requests only switch before the first token arrives.")}</small>
                </span>
              </label>
              <button className="extension-inject" type="button" disabled={!ready || applying} onClick={() => onInject(route)}>
                {applying ? <span className="spinner light" /> : (ready ? <Check size={15} weight="bold" /> : <PuzzlePiece size={16} />)}
                {applying ? t("Injecting") : (ready ? t("Transfer to VS Code extension") : t("Connections must be completed"))}
              </button>
            </article>
          );
        })}
      </div>
      <section className="extension-legend">
        <div><div className="label">{t("Before transfer")}</div><p>{t("The config is backed up, writes are atomic and an error rolls back automatically.")}</p></div>
        <div><div className="label">{t("After transfer")}</div><p>{t("The Turnout Agent restarts; the editor reads the new route through the gateway.")}</p></div>
        <div><div className="label">{t("Rollback")}</div><p><code>turnout rollback</code> {t("restores the shared config from the last backup.")}</p></div>
      </section>
    </main>
  );
}

const CONNECTION_PRESETS = {
  evren: { id: "evren", baseUrl: "", dialect: "openai-chat" },
  openai: { id: "openai", baseUrl: "", dialect: "openai-responses" },
  anthropic: { id: "anthropic", baseUrl: "", dialect: "anthropic-messages" },
  custom: { id: "", baseUrl: "", dialect: "openai-chat" },
};

function ConnectionsPage({ connections, oauthConnections, loading, onProvidersChange, onOAuthChange, showToast }) {
  const t = useT();
  const [form, setForm] = useState({ preset: "evren", ...CONNECTION_PRESETS.evren, apiKey: "" });
  const [saving, setSaving] = useState(false);
  const [oauthStarting, setOauthStarting] = useState("");

  const choosePreset = (preset) => setForm({ preset, ...CONNECTION_PRESETS[preset], apiKey: "" });
  const updateField = (field, value) => setForm((current) => ({ ...current, [field]: value }));

  const saveConnection = async (event) => {
    event.preventDefault();
    setSaving(true);
    try {
      const response = await fetch("/api/turnout/providers", {
        method: "POST",
        headers: { "Content-Type": "application/json", "X-Turnout-UI": "1" },
        body: JSON.stringify(form),
      });
      const result = await response.json().catch(() => ({}));
      if (!response.ok) throw new Error(result.error || `HTTP ${response.status}`);
      onProvidersChange(result.providers || []);
      setForm((current) => ({ ...current, apiKey: "" }));
      showToast(`${form.id}: ${t("connection saved securely")}`);
    } catch (error) {
      showToast(`${t("Could not save")}: ${error.message}`, 5000);
    } finally {
      setSaving(false);
    }
  };

  const removeConnection = async (id) => {
    if (!window.confirm(`${id} ${t("connection and its stored key will be removed. Continue?")}`)) return;
    try {
      const response = await fetch(`/api/turnout/providers/${encodeURIComponent(id)}`, {
        method: "DELETE",
        headers: { "X-Turnout-UI": "1" },
      });
      const result = await response.json().catch(() => ({}));
      if (!response.ok) throw new Error(result.error || `HTTP ${response.status}`);
      onProvidersChange(result.providers || []);
      showToast(`${id}: ${t("connection removed")}`);
    } catch (error) {
      showToast(`${t("Could not remove")}: ${error.message}`, 5000);
    }
  };

  const startOAuth = async (id) => {
    setOauthStarting(id);
    try {
      const response = await fetch(`/api/turnout/oauth/${id}/login`, {
        method: "POST",
        headers: { "X-Turnout-UI": "1" },
      });
      const result = await response.json().catch(() => ({}));
      if (!response.ok) throw new Error(result.error || `HTTP ${response.status}`);
      showToast(`${id}: ${t("official sign-in page opened")}`, 4000);
      for (let attempt = 0; attempt < 60; attempt += 1) {
        await new Promise((resolvePromise) => window.setTimeout(resolvePromise, 2000));
        const statusResponse = await fetch("/api/turnout/oauth");
        const status = await statusResponse.json().catch(() => ({}));
        if (statusResponse.ok) onOAuthChange(status.connections || []);
        const current = status.connections?.find((connection) => connection.id === id);
        const ready = id === "openai" ? current?.gateway_capable : current?.logged_in;
        if (ready) {
          const providersResponse = await fetch("/api/turnout/providers");
          const providers = await providersResponse.json().catch(() => ({}));
          if (providersResponse.ok) onProvidersChange(providers.providers || []);
          showToast(`${id}: ${t("OAuth connection ready")}`);
          return;
        }
      }
      showToast(`${id}: ${t("sign-in not finished yet")}`, 5000);
    } catch (error) {
      showToast(`${t("Could not start OAuth")}: ${error.message}`, 5000);
    } finally {
      setOauthStarting("");
    }
  };

  return (
    <main className="content connections-page">
      <div className="connections-layout">
        <div className="connections-panel">
          <section className="panel-card">
            <div className="panel-heading">
              <div><h2>{t("Official OAuth accounts")}</h2><p>{t("Codex and Claude Code use their own secure sessions; Turnout never copies the token.")}</p></div>
              <span className="status-pill">{oauthConnections.filter((item) => item.logged_in).length} {t("Connected")}</span>
            </div>
            <div className="oauth-connections">
              {oauthConnections.map((connection) => {
                const name = connection.id === "openai" ? "OpenAI / ChatGPT" : "Anthropic / Claude";
                const Icon = connection.id === "openai" ? CirclesThreePlus : SunDim;
                const isAnthropic = connection.id === "anthropic";
                const routeReady = connection.id === "openai" ? connection.gateway_capable : connection.logged_in;
                const anthropicKey = isAnthropic && Boolean(connections.find((item) => item.id === "anthropic")?.has_key);
                const connectionText = connection.id === "openai" && connection.native_logged_in && !connection.gateway_capable
                  ? t("Codex connected · separate authorization needed for Claude")
                  : (connection.account || connection.detail);
                return (
                  <article className="oauth-card" key={connection.id}>
                    <span className={`connection-icon ${connection.id}`}><Icon size={19} /></span>
                    <div className="connection-copy">
                      <strong>{name}</strong>
                      <span>{connectionText}</span>
                      <small>{connection.harness} · {connection.plan || "official account"}</small>
                      {isAnthropic && (
                        <div className="oauth-scope">
                          <Warning size={14} />
                          <span>{t("The subscription only works inside Claude Code. Codex, OpenCode and the gateway need an API key:")} <b className={anthropicKey ? "ok" : "missing"}>{anthropicKey ? t("Key ready") : t("No key")}</b></span>
                        </div>
                      )}
                    </div>
                    {isAnthropic && routeReady ? (
                      <div className="oauth-actions">
                        <span className="status-pill limited">{t("Claude Code only")}</span>
                        {!anthropicKey && <button className="oauth-button" type="button" onClick={() => choosePreset("anthropic")}>{t("Add API key")}</button>}
                      </div>
                    ) : routeReady ? <span className="status-pill">{connection.id === "openai" ? t("OpenAI ready") : t("OAuth connected")}</span> : <button className="oauth-button" type="button" disabled={!connection.installed || oauthStarting === connection.id} onClick={() => startOAuth(connection.id)}>{oauthStarting === connection.id ? t("Waiting for browser…") : (connection.id === "openai" ? t("Continue with ChatGPT") : t("Connect with account"))}</button>}
                  </article>
                );
              })}
            </div>
          </section>

          <section className="panel-card">
            <div className="panel-heading">
              <div><h2>{t("Saved providers")}</h2><p>{t("Only connections with a key can be used for routing.")}</p></div>
              <span className="status-pill">{connections.filter((item) => item.has_key).length} hazır</span>
            </div>
            <div className="connection-list">
              {loading && <div className="connection-empty">{t("Reading connections…")}</div>}
              {!loading && connections.length === 0 && <div className="connection-empty">{t("No provider added yet.")}</div>}
              {connections.map((connection) => {
                const knownName = Object.keys(PROVIDERS).find((name) => providerId(name) === connection.id);
                const entry = knownName ? PROVIDERS[knownName] : null;
                const Icon = entry?.icon ?? LinkSimple;
                return (
                  <article className="connection-card" key={connection.id}>
                    <span className={`connection-icon ${entry?.id ?? ""}`}><Icon size={19} /></span>
                    <div className="connection-copy"><strong>{entry?.name ?? connection.label}</strong><span className="mono-url">{connection.base_url}</span></div>
                    <span className="dialect-chip">{connection.dialect}</span>
                    <span className={`status-pill ${connection.has_key ? "" : "limited"}`}>{connection.has_key ? t("Key ready") : t("No key")}</span>
                    <button className="remove-connection" type="button" onClick={() => removeConnection(connection.id)}>{t("Remove")}</button>
                  </article>
                );
              })}
            </div>
            <div className="oauth-note">
              <ShieldCheck size={17} />
              <p><b>{t("Claude subscription is Claude Code only.")}</b> {t("Anthropic does not allow Pro/Max OAuth tokens in third-party apps; Turnout never copies that token. To use Claude models in Codex or OpenCode, add a separately billed Anthropic API key.")}</p>
            </div>
          </section>
        </div>

        <form className="connection-form" onSubmit={saveConnection}>
          <div className="form-head"><h2>{t("Add provider")}</h2><p>{t("Pick a ready-made service or enter an OpenAI/Anthropic compatible endpoint.")}</p></div>
          <label><span>{t("Provider")}</span><select value={form.preset} onChange={(event) => choosePreset(event.target.value)}><option value="evren">EVREN</option><option value="openai">OpenAI</option><option value="anthropic">Anthropic</option><option value="custom">{t("Custom provider")}</option></select></label>
          <label><span>{t("Identity")}</span><input value={form.id} disabled={form.preset !== "custom"} onChange={(event) => updateField("id", event.target.value.toLowerCase())} placeholder="e.g. openrouter" /></label>
          <label><span>{t("API base URL")}</span><input value={form.baseUrl} disabled={form.preset !== "custom"} onChange={(event) => updateField("baseUrl", event.target.value)} placeholder={form.preset === "custom" ? "https://api.example.com/v1" : t("Ready-made service address will be used")} /></label>
          <label><span>{t("Protocol")}</span><select value={form.dialect} disabled={form.preset !== "custom"} onChange={(event) => updateField("dialect", event.target.value)}><option value="openai-chat">OpenAI Chat</option><option value="openai-responses">OpenAI Responses</option><option value="anthropic-messages">Anthropic Messages</option></select></label>
          <label><span>{t("API key")}</span><input type="password" autoComplete="new-password" value={form.apiKey} onChange={(event) => updateField("apiKey", event.target.value)} placeholder={t("The key is written only to the data folder")} /></label>
          <button className="save-connection" type="submit" disabled={saving || !form.id || !form.apiKey}>{saving ? <span className="spinner light" /> : <LockKey size={15} />}{saving ? t("Saving") : t("Save connection")}</button>
          <p className="form-security">{t("The key is never written to browser storage, the Turnout settings file or the logs.")}</p>
          <div className="credentials-path">
            <span className="label">{t("Where it is written")}</span>
            <span className="path"><File size={13} />{t("Turnout data folder/credentials.json")}<span className="perm">0600</span></span>
          </div>
        </form>
      </div>
    </main>
  );
}

// Shared loader for pages backed by a CLI command (`turnout profiles|history`).
function useCliData(path) {
  const [data, setData] = useState(null);
  const [error, setError] = useState("");
  const [loading, setLoading] = useState(true);
  const load = useCallback(async () => {
    setLoading(true);
    setError("");
    try {
      const response = await fetch(path);
      const result = await response.json().catch(() => ({}));
      if (!response.ok) throw new Error(result.error || `HTTP ${response.status}`);
      setData(result);
    } catch (loadError) {
      setError(loadError.message);
    } finally {
      setLoading(false);
    }
  }, [path]);
  useEffect(() => { load(); }, [load]);
  return { data, error, loading, reload: load };
}

const HARNESS_NAMES = { codex: "Codex", "claude-code": "Claude Code", opencode: "OpenCode" };
const harnessName = (id) => HARNESS_NAMES[id] ?? id;

function RouteSummary({ route }) {
  const t = useT();
  const models = route.models?.length ? route.models : [route.model];
  const mode = route.hybrid ? t("hybrid") : (route.via_gateway ? t("via gateway") : t("direct"));
  return (
    <div className="route-summary">
      <div className="route-summary-head">
        <code className="mono-id">{route.model}</code>
        <span className="status-pill soft">{models.length} {t("models")}</span>
        <span className="dialect-chip">{mode}</span>
      </div>
      <div className="extension-models">
        {models.map((model) => <span className="model-chip" key={model}>{model}</span>)}
      </div>
    </div>
  );
}

function ProfilesPage({ showToast }) {
  const t = useT();
  const { data, error, loading, reload } = useCliData("/api/turnout/profiles");
  const copy = async (command) => {
    try {
      await navigator.clipboard.writeText(command);
      showToast(t("Copied"));
    } catch {
      // Clipboard permission can be denied; the command is visible anyway.
      showToast(command, 6000);
    }
  };
  return (
    <main className="content profiles-page">
      <div className="panel-card">
        <div className="panel-heading">
          <div>
            <h2>{t("Launch profiles")}</h2>
            <p>{t("A profile is injected only into the process started with it; the shared config is not touched.")}</p>
          </div>
          <button className="ghost-button" type="button" onClick={reload} disabled={loading}>
            <ArrowCounterClockwise size={15} /> {t("Refresh")}
          </button>
        </div>
        {loading && <div className="connection-empty">{t("Loading…")}</div>}
        {error && <div className="connection-empty">{t("Could not read")}: {error}</div>}
        {data?.harnesses?.map((harness) => (
          <section className="profile-harness" key={harness.harness}>
            <div className="profile-harness-head">
              <h3>{harnessName(harness.harness)}</h3>
              <span className={`status-pill ${harness.applied ? "" : "soft"}`}>
                {harness.applied ? t("Applied route") : t("No applied route")}
              </span>
            </div>
            {harness.applied && <RouteSummary route={harness.applied} />}
            {harness.profiles.length === 0 ? (
              <p className="profile-empty">{t("No saved profiles")}</p>
            ) : (
              <div className="profile-list">
                {harness.profiles.map((profile) => (
                  <article className="profile-card" key={profile.name}>
                    <div className="profile-card-head">
                      <strong>{profile.name}</strong>
                      <code>{profile.route.model}</code>
                    </div>
                    <RouteSummary route={profile.route} />
                    <div className="profile-command">
                      <code>{profile.command}</code>
                      <button className="ghost-button" type="button" onClick={() => copy(profile.command)}>
                        {t("Copy command")}
                      </button>
                    </div>
                  </article>
                ))}
              </div>
            )}
          </section>
        ))}
        {data?.data_dir && <p className="security-note"><File size={13} /><span className="mono">{data.data_dir}</span></p>}
      </div>
    </main>
  );
}

function formatDate(unix) {
  if (!unix) return "—";
  return new Date(unix * 1000).toLocaleString(undefined, {
    year: "numeric", month: "2-digit", day: "2-digit", hour: "2-digit", minute: "2-digit",
  });
}

function LogsPage({ showToast }) {
  const t = useT();
  const { data, error, loading, reload } = useCliData("/api/turnout/history?limit=100");
  const restore = async () => {
    if (!window.confirm(t("Restore the last applied plan? The shared config is restored from the last backup."))) return;
    try {
      const response = await fetch("/api/turnout/rollback", {
        method: "POST",
        headers: { "X-Turnout-UI": "1" },
      });
      const result = await response.json().catch(() => ({}));
      if (!response.ok) throw new Error(result.error || `HTTP ${response.status}`);
      showToast(result.detail ? result.detail.split("\n")[0] : t("Rollback"));
      reload();
    } catch (rollbackError) {
      showToast(`${t("Could not read")}: ${rollbackError.message}`, 5000);
    }
  };

  // The newest Agent line is what the background service is serving right now.
  const entries = data?.log?.entries ?? [];
  const backups = data?.backups ?? [];
  const requests = data?.requests ?? [];
  const tokensIn = requests.reduce((sum, request) => sum + (request.input_tokens || 0), 0);
  const tokensOut = requests.reduce((sum, request) => sum + (request.output_tokens || 0), 0);
  const failures = requests.filter((request) => request.status === "error").length;
  const fallbacks = requests.filter((request) => request.fallback).length;
  return (
    <main className="content logs-page">
      <section className="panel-card">
        <div className="panel-heading">
          <div>
            <h2>{t("Requests")}</h2>
            <p>{t("What the gateway actually served: model, provider, duration and tokens. Prompts and replies are never recorded.")}</p>
          </div>
          <button className="ghost-button" type="button" onClick={reload} disabled={loading}>
            <ArrowCounterClockwise size={15} /> {t("Refresh")}
          </button>
        </div>
        {requests.length > 0 && (
          <div className="request-totals">
            <span><b>{requests.length}</b> {t("requests")}</span>
            <span><b>{tokensIn}</b> {t("input tokens")}</span>
            <span><b>{tokensOut}</b> {t("output tokens")}</span>
            {fallbacks > 0 && <span className="amber"><b>{fallbacks}</b> {t("fallbacks")}</span>}
            {failures > 0 && <span className="negative"><b>{failures}</b> {t("failed")}</span>}
          </div>
        )}
        {loading && <div className="connection-empty">{t("Loading…")}</div>}
        {!loading && !error && requests.length === 0 && (
          <div className="connection-empty">{t("No requests yet")}</div>
        )}
        {requests.length > 0 && (
          <table className="history-table requests-table">
            <thead>
              <tr>
                <th>{t("Date")}</th>
                <th>{t("Harness")}</th>
                <th>{t("Model")}</th>
                <th>{t("Served by")}</th>
                <th>{t("Duration")}</th>
                <th>{t("Tokens")}</th>
              </tr>
            </thead>
            <tbody>
              {requests.map((request, index) => (
                <tr key={`${request.ts}-${index}`}>
                  <td>{formatDate(request.ts)}</td>
                  <td><span className="status-pill soft">{request.harness}</span></td>
                  <td><code>{request.model}</code></td>
                  <td>
                    <code>{request.provider}:{request.upstream_model}</code>
                    {" "}
                    {request.fallback && <span className="status-pill amber">{t("fallback")}</span>}
                    {request.status === "error" && <span className="status-pill negative" title={request.error || ""}>{t("failed")}</span>}
                  </td>
                  <td className="mono">
                    {request.ms} ms
                    {request.first_frame_ms != null && request.first_frame_ms < request.ms && (
                      <small>· {request.first_frame_ms} ms {t("to first token")}</small>
                    )}
                  </td>
                  <td className="mono">{request.input_tokens} / {request.output_tokens}</td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>

      <section className="panel-card">
        <div className="panel-heading">
          <div>
            <h2>{t("Agent log")}</h2>
            <p>{t("One line per started route; the only record of what the background service served.")}</p>
          </div>
          <button className="ghost-button" type="button" onClick={reload} disabled={loading}>
            <ArrowCounterClockwise size={15} /> {t("Refresh")}
          </button>
        </div>
        {loading && <div className="connection-empty">{t("Loading…")}</div>}
        {error && <div className="connection-empty">{t("Could not read")}: {error}</div>}
        {!loading && !error && entries.length === 0 && (
          <div className="connection-empty">{t("No log lines yet")}</div>
        )}
        {entries.length > 0 && (
          <ol className="log-lines">
            {entries.slice(-150).reverse().map((line, index) => (
              <li key={`${index}-${line}`}><code>{line}</code></li>
            ))}
          </ol>
        )}
        {data?.log?.path && <p className="security-note"><File size={13} /><span className="mono">{data.log.path}</span></p>}
      </section>

      <section className="panel-card">
        <div className="panel-heading">
          <div>
            <h2>{t("Backups")}</h2>
            <p>{t("Every apply takes a backup; rollback restores these files.")}</p>
          </div>
          <button className="ghost-button" type="button" onClick={restore}>
            <ArrowLeft size={15} /> {t("Restore the last applied plan")}
          </button>
        </div>
        {!loading && !error && backups.length === 0 && (
          <div className="connection-empty">{t("No backups yet")}</div>
        )}
        {backups.length > 0 && (
          <table className="history-table">
            <thead>
              <tr>
                <th>{t("Backup")}</th>
                <th>{t("Label")}</th>
                <th>{t("Date")}</th>
                <th>{t("Files")}</th>
              </tr>
            </thead>
            <tbody>
              {backups.map((backup) => (
                <tr key={backup.id}>
                  <td><code>{backup.id}</code></td>
                  <td>{backup.label}</td>
                  <td>{formatDate(backup.created_unix)}</td>
                  <td>
                    <details className="history-files">
                      <summary>{backup.files.length}</summary>
                      <ul>
                        {backup.files.map((file) => (
                          <li key={file.path}>
                            <span className="mono">{file.path}</span>
                            {!file.existed && <span className="status-pill soft">new</span>}
                          </li>
                        ))}
                      </ul>
                    </details>
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        )}
      </section>
    </main>
  );
}

function SettingsPage({ agentRunning, showToast }) {
  const t = useT();
  const language = useLanguage();
  const setLanguage = useSetLanguage();
  const { data } = useCliData("/api/turnout/profiles");
  return (
    <main className="content settings-page">
      <section className="panel-card">
        <div className="panel-heading">
          <div><h2>{t("Interface language")}</h2><p>{t("The interface language is remembered on this computer.")}</p></div>
        </div>
        <div className="language-choice">
          {LANGUAGES.map((option) => (
            <label key={option.id} className={`language-option ${language === option.id ? "selected" : ""}`}>
              <input
                type="radio"
                name="language"
                value={option.id}
                checked={language === option.id}
                onChange={() => setLanguage(option.id)}
              />
              <span><b>{option.label}</b><small>{option.short}</small></span>
            </label>
          ))}
        </div>
      </section>

      <section className="panel-card">
        <div className="panel-heading">
          <div><h2>{t("Data folder")}</h2><p className="mono">{data?.data_dir ?? "—"}</p></div>
          <span className={`status-pill ${agentRunning ? "" : "offline"}`}>
            <span className={`dot ${agentRunning ? "" : "offline"}`} />
            {t("Turnout Agent")}: {agentRunning ? t("Running") : t("Stopped")}
          </span>
        </div>
        <div className="settings-commands">
          <div className="profile-command">
            <code>turnout</code>
            <span>{t("Full-screen CLI")}</span>
            <button
              className="ghost-button"
              type="button"
              onClick={async () => {
                try {
                  await navigator.clipboard.writeText("turnout");
                  showToast(t("Copied"));
                } catch {
                  showToast("turnout", 4000);
                }
              }}
            >
              {t("Copy command")}
            </button>
          </div>
          <div className="profile-command">
            <code>turnout gateway install</code>
            <span>{t("Turnout Agent")}</span>
          </div>
        </div>
      </section>
    </main>
  );
}

function CommandPalette({ routes, selectedId, onClose, onSelect, onCommand, onModel }) {
  const t = useT();
  const [value, setValue] = useState("");
  const needle = value.toLowerCase();
  const selectedRoute = routes.find((route) => route.id === selectedId) ?? routes[0];
  const filteredRoutes = routes.filter((route) => route.name.toLowerCase().includes(needle));
  const commands = [
    { id: "test", label: t("Test the connection"), hint: t("Send a real request to the selected route"), keys: "t", Icon: Play },
    { id: "apply", label: t("Apply the plan"), hint: t("Show the diff, back up, write"), keys: "a", Icon: ArrowRight },
    { id: "rollback", label: t("Rollback"), hint: t("Revert the last applied plan"), keys: "b", Icon: ArrowLeft },
  ].filter((command) => command.label.toLowerCase().includes(needle));
  const models = selectedRoute.targets.filter((target) => target.model.toLowerCase().includes(needle));
  useEffect(() => {
    const close = (event) => event.key === "Escape" && onClose();
    window.addEventListener("keydown", close);
    return () => window.removeEventListener("keydown", close);
  }, [onClose]);
  return (
    <div className="modal-backdrop" role="presentation" onMouseDown={onClose}>
      <div className="command-palette" role="dialog" aria-modal="true" aria-label={t("Search or run a command")} onMouseDown={(event) => event.stopPropagation()}>
        <div className="palette-search"><MagnifyingGlass size={18} /><input aria-label={t("Search or run a command")} autoFocus value={value} onChange={(event) => setValue(event.target.value)} placeholder={t("Search or run a command")} /><button type="button" aria-label={t("Close search")} onClick={onClose}><X size={16} /></button></div>
        <div className="palette-results">
          {commands.length > 0 && <span className="palette-label">{t("Commands")}</span>}
          {commands.map(({ id, label, hint, keys, Icon }) => (
            <button type="button" key={id} onClick={() => { onCommand(id); onClose(); }}>
              <Icon size={17} />
              <span className="palette-copy"><strong>{label}</strong><small>{hint}</small></span>
              <kbd className="palette-key">{keys}</kbd>
            </button>
          ))}
          {filteredRoutes.length > 0 && <span className="palette-label">{t("Harness")}</span>}
          {filteredRoutes.map((route) => {
            const Icon = route.Icon;
            return (
              <button type="button" key={route.id} onClick={() => { onSelect(route.id); onClose(); }}>
                <span className="palette-mark"><Icon size={13} /></span>
                <span className="palette-copy"><strong>{route.name}</strong><small>{route.provider} · {route.model}</small></span>
              </button>
            );
          })}
          {models.length > 0 && <span className="palette-label">{t("Models")}</span>}
          {models.map((target) => (
            <button type="button" key={`${target.provider}::${target.model}`} onClick={() => { onModel(target); onClose(); }}>
              <span className={`model-mark ${providerId(target.provider)}`} aria-hidden="true">{PROVIDER_LETTER[providerId(target.provider)] ?? "?"}</span>
              <span className="palette-copy"><strong className="mono-id">{target.model}</strong><small>{target.provider}{selectedRoute.provider === target.provider && selectedRoute.model === target.model ? ` · ${t("Default")}` : ""}</small></span>
            </button>
          ))}
        </div>
        <div className="palette-footer"><span><kbd>↑↓</kbd> {t("navigate")}</span><span><kbd>⏎</kbd> {t("select")}</span><span><kbd>⌘K</kbd> {t("close")}</span></div>
      </div>
    </div>
  );
}

export function App() {
  const [language, setLanguageState] = useState(() => {
    try {
      const saved = window.localStorage.getItem(LANGUAGE_STORAGE_KEY);
      return LANGUAGES.some((option) => option.id === saved) ? saved : "tr";
    } catch {
      return "tr";
    }
  });
  const setLanguage = useCallback((next) => {
    setLanguageState(next);
    try { window.localStorage.setItem(LANGUAGE_STORAGE_KEY, next); } catch { /* private mode */ }
    document.documentElement.lang = next;
  }, []);
  useEffect(() => { document.documentElement.lang = language; }, [language]);
  const [activePage, setActivePage] = useState(() => {
    const requested = new URLSearchParams(window.location.search).get("page");
    return NAV_ITEMS.some((item) => item.id === requested) ? requested : "routing";
  });
  const [routes, setRoutes] = useState(initialRoutes);
  const [selectedId, setSelectedId] = useState("opencode");
  const [testState, setTestState] = useState("idle");
  // Last test per route, kept in this browser only: a result must survive
  // leaving the screen, and must never be written into the live Turnout data.
  const [testRecords, setTestRecords] = useState(() => {
    try {
      const saved = JSON.parse(window.localStorage.getItem(TESTS_STORAGE_KEY) ?? "{}");
      return saved && typeof saved === "object" ? saved : {};
    } catch {
      return {};
    }
  });
  const [routing, setRouting] = useState({ status: "loading", byRoute: {}, readAt: null, error: "" });
  const [applyState, setApplyState] = useState("idle");
  const [paletteOpen, setPaletteOpen] = useState(false);
  const [toast, setToast] = useState("");
  const [connections, setConnections] = useState([]);
  const [oauthConnections, setOauthConnections] = useState([]);
  const [connectionsLoading, setConnectionsLoading] = useState(true);
  const [agentRunning, setAgentRunning] = useState(false);

  const t = useCallback((key) => translate(language, key), [language]);
  const selectedRoute = useMemo(() => routes.find((route) => route.id === selectedId) ?? routes[0], [routes, selectedId]);

  useEffect(() => {
    const openPalette = (event) => {
      if ((event.metaKey || event.ctrlKey) && event.key.toLowerCase() === "k") {
        event.preventDefault();
        setPaletteOpen(true);
      }
    };
    window.addEventListener("keydown", openPalette);
    return () => window.removeEventListener("keydown", openPalette);
  }, []);

  useEffect(() => {
    let active = true;
    Promise.all([
      fetch("/api/turnout/providers"),
      fetch("/api/turnout/oauth"),
      fetch("/api/turnout/agent"),
    ])
      .then(async ([providerResponse, oauthResponse, agentResponse]) => {
        const providerResult = await providerResponse.json().catch(() => ({}));
        const oauthResult = await oauthResponse.json().catch(() => ({}));
        const agentResult = await agentResponse.json().catch(() => ({}));
        if (!providerResponse.ok) throw new Error(providerResult.error || `HTTP ${providerResponse.status}`);
        if (!oauthResponse.ok) throw new Error(oauthResult.error || `HTTP ${oauthResponse.status}`);
        if (active) {
          setConnections(providerResult.providers || []);
          setOauthConnections(oauthResult.connections || []);
          setAgentRunning(Boolean(agentResult.running));
        }
      })
      .catch((error) => {
        if (active) setToast(`${t("Could not read connections")}: ${error.message}`);
      })
      .finally(() => { if (active) setConnectionsLoading(false); });
    return () => { active = false; };
  }, []);

  useEffect(() => {
    const serializable = routes.map(({ Icon: _Icon, ...route }) => route);
    window.localStorage.setItem(ROUTES_STORAGE_KEY, JSON.stringify(serializable));
  }, [routes]);

  useEffect(() => {
    try { window.localStorage.setItem(TESTS_STORAGE_KEY, JSON.stringify(testRecords)); } catch { /* private mode */ }
  }, [testRecords]);

  // Read-only: `turnout profiles --json` is the single source for every route
  // number on this screen. Local edits are replaced by the saved state.
  const loadRouting = useCallback(async () => {
    setRouting((current) => ({ ...current, status: "loading", error: "" }));
    try {
      const response = await fetch("/api/turnout/profiles");
      const result = await response.json().catch(() => ({}));
      if (!response.ok) throw new Error(result.error || `HTTP ${response.status}`);
      const byRoute = routingFromProfiles(result);
      setRoutes((current) => current.map((route) => {
        const saved = byRoute[route.id]?.saved;
        return saved
          ? { ...route, targets: saved.targets, provider: saved.provider, model: saved.model, fallback: saved.fallback }
          : route;
      }));
      setRouting({ status: "ready", byRoute, readAt: new Date().toISOString(), error: "" });
    } catch (error) {
      setRouting((current) => ({ ...current, status: "error", error: error.message }));
    }
  }, []);

  useEffect(() => { loadRouting(); }, [loadRouting]);

  const pendingById = useMemo(() => Object.fromEntries(
    routes.map((route) => [route.id, pendingChanges(route, routing.byRoute[route.id]?.saved)]),
  ), [routes, routing.byRoute]);

  const updateRoute = (id, patch) => {
    setRoutes((current) => current.map((route) => route.id === id ? { ...route, ...patch } : route));
    setTestState("idle");
  };

  const handleModelChange = (id, patch) => updateRoute(id, patch);

  const showToast = (message, duration = 2800) => {
    setToast(message);
    window.setTimeout(() => setToast(""), duration);
  };

  const markTested = (id) => {
    setRoutes((current) => current.map((route) => route.id === id ? { ...route, lastTestAt: new Date().toISOString() } : route));
  };

  const recordTest = (route, ok, detail) => {
    setTestRecords((current) => ({
      ...current,
      [route.id]: { ok, model: route.model, detail, at: new Date().toISOString() },
    }));
  };

  const runTest = async () => {
    if (routing.status !== "ready") return;
    const route = selectedRoute;
    setTestState("testing");
    try {
      const oauth = oauthConnection(oauthConnections, selectedRoute.provider);
      const nativeOAuth = (selectedRoute.id === "codex" && providerId(selectedRoute.provider) === "openai")
        || (selectedRoute.id === "claude" && providerId(selectedRoute.provider) === "anthropic");
      if (nativeOAuth && oauth?.logged_in && !providerConnection(connections, selectedRoute.provider)?.has_key) {
        const response = await fetch("/api/turnout/oauth");
        const result = await response.json().catch(() => ({}));
        if (!response.ok) throw new Error(result.error || `HTTP ${response.status}`);
        const current = result.connections?.find((connection) => connection.id === oauth.id);
        setOauthConnections(result.connections || []);
        if (!current?.logged_in) throw new Error(t("OAuth session is no longer active"));
        recordTest(route, true, `${t("Official session verified")} · ${current.harness} · ${current.plan || current.detail}`);
        setTestState("success");
        markTested(route.id);
        return;
      }
      const response = await fetch("/api/turnout/providers/test", {
        method: "POST",
        headers: { "Content-Type": "application/json", "X-Turnout-UI": "1" },
        body: JSON.stringify({ provider: providerId(selectedRoute.provider), model: selectedRoute.model }),
      });
      const result = await response.json().catch(() => ({}));
      if (!response.ok) throw new Error(result.error || `HTTP ${response.status}`);
      recordTest(route, true, `${result.latency} ms · ${result.detail}`);
      setTestState("success");
      markTested(route.id);
    } catch (error) {
      recordTest(route, false, error.message);
      setTestState("error");
    }
  };

  const applyConfiguration = async (surface = "cli", route = selectedRoute) => {
    setApplyState(`${surface}:${route.id}`);
    try {
      const response = await fetch("/api/turnout/routes/apply", {
        method: "POST",
        headers: { "Content-Type": "application/json", "X-Turnout-UI": "1" },
        body: JSON.stringify({
          harness: route.id,
          surface,
          defaultTarget: { provider: route.provider, model: route.model },
          targets: route.targets,
          hybrid: surface === "vscode" && canKeepOriginalList(route) && route.keepOriginal !== false,
          fallback: route.fallback !== false,
        }),
      });
      const result = await response.json().catch(() => ({}));
      if (!response.ok) throw new Error(result.error || `HTTP ${response.status}`);
      setApplyState("idle");
      // Re-read so the screen shows what was actually saved, not what was sent.
      loadRouting();
      showToast(
        result.mode === "cli-profile"
          ? `${route.name}: CLI profili kaydedildi — ${result.command}`
          : result.mode === "hybrid"
            ? `${route.name}: ${t("transferred to VS Code; original list kept")}`
            : result.mode === "oauth"
              ? (surface === "cli"
                ? `${route.name}: ${t("official OAuth session ready; config not changed")}`
                : `${route.name}: ${t("official OAuth route applied to VS Code")}`)
              : `${route.name}: ${result.models} ${t("models transferred to the VS Code extension")}`,
        result.mode === "cli-profile" ? 7000 : undefined,
      );
    } catch (error) {
      setApplyState("idle");
      showToast(`${t("Could not apply")}: ${error.message}`, 5000);
    }
  };

  const rollback = async () => {
    if (!window.confirm(t("Restore the last applied plan? The shared config is restored from the last backup."))) return;
    try {
      const response = await fetch("/api/turnout/rollback", {
        method: "POST",
        headers: { "X-Turnout-UI": "1" },
      });
      const result = await response.json().catch(() => ({}));
      if (!response.ok) throw new Error(result.error || `HTTP ${response.status}`);
      showToast(result.detail ? `${t("Rolled back")}: ${result.detail.split("\n")[0]}` : t("Last plan rolled back"));
      setTestState("idle");
      loadRouting();
    } catch (error) {
      showToast(`${t("Could not roll back")}: ${error.message}`, 5000);
    }
  };

  const readAt = routing.readAt
    ? new Date(routing.readAt).toLocaleTimeString(language === "tr" ? "tr-TR" : undefined, { hour: "2-digit", minute: "2-digit" })
    : "";

  return (
    <LanguageContext.Provider value={language}>
    <LanguageUpdateContext.Provider value={setLanguage}>
    <div className="app-shell">
      <TopBar onSearch={() => setPaletteOpen(true)} page={activePage} agentRunning={agentRunning} />
      <Sidebar active={activePage} onChange={setActivePage} />
      {activePage === "routing" ? (
        <main className="content">
          <ProvidersBar connections={connections} oauthConnections={oauthConnections} loading={connectionsLoading} />
          <div className="workspace-grid">
            <section className="matrix-panel">
              <div className="matrix-title">
                <div>
                  <h2>{t("Routing matrix")}</h2>
                  <p className="source-caption">
                    {routing.status === "ready" ? <>{t("Source")}: <code>turnout profiles</code> · {readAt}</>
                      : routing.status === "loading" ? <>{t("Source")}: <code>turnout profiles</code> · {t("Reading…")}</>
                        : <>{t("Source")}: <code>turnout profiles</code> · <span className="unknown-text">{t("unknown")}</span></>}
                  </p>
                </div>
                <button className="ghost-button" type="button" onClick={loadRouting} disabled={routing.status === "loading"}><ArrowCounterClockwise size={15} /> {t("Refresh")}</button>
              </div>
              {routing.status === "error" && (
                <div className="routing-alert" role="alert">
                  <WarningCircle size={20} weight="fill" />
                  <div>
                    <strong>{t("Could not read the routing state")}</strong>
                    <span>{routing.error}. {t("Run `turnout profiles` in a terminal; if it fails, restart the gateway with `turnout gateway start`.")}</span>
                    <div><button className="ghost-button" type="button" onClick={loadRouting}>{t("Try again")}</button></div>
                  </div>
                </div>
              )}
              <RoutingTable routes={routes} connections={connections} oauthConnections={oauthConnections} selectedId={selectedId} onSelect={(id) => { setSelectedId(id); setTestState("idle"); }} onModelChange={handleModelChange} connectionsLoading={connectionsLoading} routingStatus={routing.status} pendingById={pendingById} />
            </section>
            <Inspector route={selectedRoute} connections={connections} oauthConnections={oauthConnections} testState={testState} applyState={applyState === `cli:${selectedRoute.id}` ? "applying" : "idle"} onTest={runTest} onApply={() => applyConfiguration("cli", selectedRoute)} connectionsLoading={connectionsLoading} routing={routing.byRoute[selectedRoute.id]} routingStatus={routing.status} pending={pendingById[selectedRoute.id] ?? 0} testRecord={testRecords[selectedRoute.id]} />
          </div>
        </main>
      ) : activePage === "connections" ? (
        <ConnectionsPage connections={connections} oauthConnections={oauthConnections} loading={connectionsLoading} onProvidersChange={(providers) => { setConnections(providers); setConnectionsLoading(false); }} onOAuthChange={setOauthConnections} showToast={showToast} />
      ) : activePage === "extensions" ? (
        <ExtensionsPage routes={routes} connections={connections} oauthConnections={oauthConnections} applyState={applyState} onInject={(route) => applyConfiguration("vscode", route)} onRouteChange={updateRoute} />
      ) : activePage === "profiles" ? (
        <ProfilesPage showToast={showToast} />
      ) : activePage === "logs" ? (
        <LogsPage showToast={showToast} />
      ) : (
        <SettingsPage agentRunning={agentRunning} showToast={showToast} />
      )}
      {paletteOpen && (
        <CommandPalette
          routes={routes}
          selectedId={selectedRoute.id}
          onClose={() => setPaletteOpen(false)}
          onSelect={(id) => { setSelectedId(id); setActivePage("routing"); }}
          onModel={(target) => {
            setActivePage("routing");
            setSelectedId(selectedRoute.id);
            handleModelChange(selectedRoute.id, { provider: target.provider, model: target.model });
          }}
          onCommand={(id) => {
            setActivePage("routing");
            if (id === "test") runTest();
            else if (id === "rollback") rollback();
            else applyConfiguration("cli", selectedRoute);
          }}
        />
      )}
      <div className={`toast ${toast ? "visible" : ""}`} role="status"><ShieldCheck size={20} weight="fill" />{toast}</div>
    </div>
    </LanguageUpdateContext.Provider>
    </LanguageContext.Provider>
  );
}
