import { useCallback, useRef, useState } from "react";
import { formatBackendError } from "../backend";

export function useGymChange(load: () => Promise<void>, onError: (error: string | null) => void) {
  const [busy, setBusy] = useState(false);
  const locked = useRef(false);
  const run = useCallback(
    async (action: () => Promise<unknown>, refresh = true): Promise<boolean> => {
      if (locked.current) return false;
      locked.current = true;
      setBusy(true);
      onError(null);
      try {
        await action();
        return true;
      } catch (caught) {
        onError(formatBackendError(caught));
        return false;
      } finally {
        try {
          if (refresh) await load();
        } finally {
          locked.current = false;
          setBusy(false);
        }
      }
    },
    [load, onError],
  );
  return { busy, run };
}
