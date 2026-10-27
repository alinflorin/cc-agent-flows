/**
 * App-wide preferences, kept in `settings.json` in the app's config directory
 * (tauri-plugin-store) so the Rust side can read them too. In a plain browser
 * (`pnpm dev`) it falls back to localStorage.
 *
 * `loadPrefs()` must finish before anything reads a value: after that, reads
 * are synchronous from an in-memory copy and writes persist in the background.
 */
export const inTauri = () => typeof window !== "undefined" && "__TAURI_INTERNALS__" in window;

const FILE = "settings.json";
const KEYS = ["theme", "language", "autoUpdate", "displayName", "llmModel", "llmThinking", "welcomed"];

type Store = { get<T>(k: string): Promise<T | undefined>; set(k: string, v: unknown): Promise<void> };

let store: Store | null = null;
const cache = new Map<string, unknown>();

/** Browser fallback: strings are stored bare, everything else as JSON. */
function fromLocalStorage(key: string): unknown {
  try {
    const raw = localStorage.getItem(key);
    if (raw === null) return undefined;
    try {
      return JSON.parse(raw);
    } catch {
      return raw;
    }
  } catch {
    return undefined;
  }
}

export async function loadPrefs(): Promise<void> {
  if (inTauri()) {
    try {
      const { load } = await import("@tauri-apps/plugin-store");
      store = await load(FILE, { defaults: {}, autoSave: 100 });
    } catch (e) {
      console.warn("preferences store unavailable, using localStorage", e);
    }
  }
  for (const key of KEYS) {
    const value = store ? await store.get(key) : fromLocalStorage(key);
    if (value !== undefined) cache.set(key, value);
  }
}

export function getPref<T>(key: string): T | undefined {
  return cache.get(key) as T | undefined;
}

export function setPref(key: string, value: unknown) {
  cache.set(key, value);
  if (store) {
    store.set(key, value).catch((e) => console.warn("could not save preference", key, e));
    return;
  }
  try {
    localStorage.setItem(key, typeof value === "string" ? value : JSON.stringify(value));
  } catch {
    // storage unavailable: the preference just won't persist
  }
}
