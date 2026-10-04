import { useCallback, useEffect, useRef, useState } from "react";
import { formatBackendError } from "./backend-error.ts";

type ErrorHandler = (error: string | null) => void;

export function useGymData<T>(load: () => Promise<T>, state: unknown, onError: ErrorHandler) {
    const [value, setValue] = useState<T | undefined>();
    const [failed, setFailed] = useState(false);
    const generation = useRef(0);
    const reload = useCallback(async () => {
        const current = ++generation.current;
        try {
            const result = await load();
            if (current !== generation.current) return;
            setValue(result);
            setFailed(false);
        } catch (caught) {
            if (current !== generation.current) return;
            setFailed(true);
            onError(formatBackendError(caught));
        }
    }, [load, onError]);
    useEffect(() => {
        void reload();
        return () => {
            generation.current += 1;
        };
    }, [reload, state]);
    return { value, failed, reload };
}

export function useGymChange(reload: () => void | Promise<void>, onError: ErrorHandler) {
    const [busy, setBusy] = useState(false);
    const busyRef = useRef(false);
    const run = useCallback(
        async (action: () => Promise<unknown>, refresh = true): Promise<boolean> => {
            if (busyRef.current) return false;
            busyRef.current = true;
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
                    if (refresh) await reload();
                } finally {
                    busyRef.current = false;
                    setBusy(false);
                }
            }
        },
        [reload, onError],
    );
    return { busy, run };
}
