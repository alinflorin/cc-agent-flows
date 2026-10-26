import { createContext, useCallback, useContext, useId, type ReactNode } from "react";
import { Toast, ToastTitle, Toaster, useToastController } from "@fluentui/react-components";

type Intent = "info" | "success" | "warning" | "error";
type Notify = (message: string, intent?: Intent) => void;

const Ctx = createContext<Notify>(() => {});

export const useNotify = () => useContext(Ctx);

/** Mounts one toaster and hands `notify(message, intent)` to the tree below. */
export function NotificationsProvider({ children }: { children: ReactNode }) {
  const toasterId = useId();
  const { dispatchToast } = useToastController(toasterId);
  const notify = useCallback<Notify>(
    (message, intent = "info") =>
      dispatchToast(
        <Toast>
          <ToastTitle>{message}</ToastTitle>
        </Toast>,
        { intent, timeout: 4000 },
      ),
    [dispatchToast],
  );
  return (
    <Ctx.Provider value={notify}>
      <Toaster toasterId={toasterId} position="bottom-end" />
      {children}
    </Ctx.Provider>
  );
}
