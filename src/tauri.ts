import { native } from "./prefs";

/**
 * Calls a backend command. In the app's window it is a Tauri `invoke` (the API is imported on demand
 * so a plain browser still loads); in the remote-control page it is `POST /api/<cmd>` to the server,
 * which runs the very same command. Errors are thrown as the command's own error value, like `invoke`.
 */
export async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  if (native()) {
    const { invoke } = await import("@tauri-apps/api/core");
    return invoke<T>(cmd, args);
  }
  const res = await fetch(`/api/${cmd}`, { method: "POST", headers: { "Content-Type": "application/json" }, body: JSON.stringify(args ?? {}) });
  const text = await res.text();
  let data: unknown = null;
  try {
    data = text ? JSON.parse(text) : null;
  } catch {
    // not JSON: reported below
  }
  if (!res.ok) throw data && typeof data === "object" && "error" in data ? (data as { error: unknown }).error : `HTTP ${res.status}`;
  return data as T;
}

let stream: EventSource | null = null;

/** Listens to an app event (`session-log`, `sessions-changed`, `config-changed`); returns the function that stops it. */
export async function listen<T = unknown>(event: string, handler: (e: { payload: T }) => void): Promise<() => void> {
  if (native()) {
    const { listen: tauriListen } = await import("@tauri-apps/api/event");
    return tauriListen<T>(event, handler);
  }
  stream ??= new EventSource("/events");
  const es = stream;
  const h = (m: Event) => handler({ payload: JSON.parse((m as MessageEvent<string>).data) as T });
  es.addEventListener(event, h);
  return () => es.removeEventListener(event, h);
}
