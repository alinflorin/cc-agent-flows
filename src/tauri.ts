/** Calls a backend command. The Tauri API is imported on demand so a plain browser (`pnpm dev`) still loads. */
export async function call<T>(cmd: string, args?: Record<string, unknown>): Promise<T> {
  const { invoke } = await import("@tauri-apps/api/core");
  return invoke<T>(cmd, args);
}
