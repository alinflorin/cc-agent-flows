import { useSyncExternalStore } from "react";

const QUERY = "(max-width: 720px)";

/** True on a narrow window, where the sidebar gives way to a menu button. */
export function useIsMobile() {
  return useSyncExternalStore(
    (cb) => {
      const mq = window.matchMedia(QUERY);
      mq.addEventListener("change", cb);
      return () => mq.removeEventListener("change", cb);
    },
    () => window.matchMedia(QUERY).matches,
  );
}
