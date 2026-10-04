import type { MobileClipperClientLike } from "@clipper/mobile-bridge";
import type { AppState } from "@clipper/shared";
import { useCallback, useEffect, useRef, useState } from "react";
import { AppState as NativeAppState, Keyboard } from "react-native";
import { backend, formatBackendError } from "../backend";

export function kitchen(): MobileClipperClientLike {
  return backend.nativeClient();
}

export function deviceZone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
  } catch {
    return "UTC";
  }
}

export function blockTime(millis: bigint): string {
  return new Date(Number(millis)).toLocaleString(undefined, {
    weekday: "long",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function localTime(millis: bigint): string {
  return new Date(Number(millis)).toLocaleString();
}

export function clock(millis: bigint): string {
  const seconds = millis > 0n ? (millis + 999n) / 1000n : 0n;
  const hours = seconds / 3600n;
  const minutes = (seconds % 3600n) / 60n;
  const rest = String(seconds % 60n).padStart(2, "0");
  return hours > 0n ? `${hours}:${String(minutes).padStart(2, "0")}:${rest}` : `${minutes}:${rest}`;
}

export function useKitchenView<T>(
  query: () => Promise<T>,
  state: AppState,
  onError: (error: string | null) => void,
) {
  const [view, setView] = useState<T | null>(null);
  const [loading, setLoading] = useState(true);
  const [failed, setFailed] = useState(false);
  const [version, setVersion] = useState(0);
  const reload = useCallback(() => setVersion((value) => value + 1), []);

  useEffect(() => {
    let cancelled = false;
    setLoading(true);
    void (async () => {
      try {
        const result = await query();
        if (cancelled) return;
        setView(result);
        setFailed(false);
      } catch (caught) {
        if (cancelled) return;
        onError(formatBackendError(caught));
        setView(null);
        setFailed(true);
      } finally {
        if (!cancelled) setLoading(false);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [query, state, version, onError]);

  useEffect(() => {
    const subscription = NativeAppState.addEventListener("change", (next) => {
      if (next === "active") reload();
    });
    return () => subscription.remove();
  }, [reload]);

  return { view, loading, failed, reload };
}

export function useKitchenChange(reload: () => void, onError: (error: string | null) => void) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const busyRef = useRef(false);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  async function run(action: () => Promise<void>): Promise<boolean> {
    if (busyRef.current) return false;
    busyRef.current = true;
    setBusy(true);
    setError(null);
    onError(null);
    Keyboard.dismiss();
    try {
      await action();
      return true;
    } catch (caught) {
      if (mounted.current) {
        const message = formatBackendError(caught);
        setError(message);
        onError(message);
      }
      return false;
    } finally {
      busyRef.current = false;
      if (mounted.current) {
        setBusy(false);
        reload();
      }
    }
  }

  return { busy, error, run };
}
