import { useLayoutEffect, useRef } from "react";

/** For a scrollable box that follows its newest content, unless the user has scrolled up to read. `deps` are what changes when there is more. */
export function useFollow<T extends HTMLElement>(...deps: unknown[]) {
  const ref = useRef<T>(null);
  const follow = useRef(true);
  useLayoutEffect(() => {
    const el = ref.current;
    if (el && follow.current) el.scrollTop = el.scrollHeight;
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, deps);
  return {
    ref,
    onScroll: (e: { currentTarget: HTMLElement }) => {
      const el = e.currentTarget;
      follow.current = el.scrollHeight - el.scrollTop - el.clientHeight < 24;
    },
  };
}

/** A `div` that follows its newest content (see `useFollow`). Give it a `key` to start over at the bottom. */
export function Follow({ deps, ...props }: { deps: unknown[] } & React.HTMLAttributes<HTMLDivElement>) {
  const follow = useFollow<HTMLDivElement>(...deps);
  return <div {...props} {...follow} />;
}
