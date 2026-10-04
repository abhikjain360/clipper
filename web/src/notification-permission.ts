import { invoke } from "@tauri-apps/api/core";
import { useEffect, useState } from "react";
import { isTauriRuntime } from "./backend";

export function useNotificationPermission(active: boolean): boolean | null {
    const [allowed, setAllowed] = useState<boolean | null>(null);
    useEffect(() => {
        if (!active || !isTauriRuntime()) {
            setAllowed(null);
            return;
        }
        let stopped = false;
        let pending = false;
        async function refresh() {
            if (pending) return;
            pending = true;
            try {
                const permission = await invoke<boolean | null>("notification_permission");
                if (!stopped) setAllowed(permission);
            } catch {
                if (!stopped) setAllowed(false);
            } finally {
                pending = false;
            }
        }
        void refresh();
        const timer = setInterval(() => void refresh(), 60_000);
        const focus = () => void refresh();
        window.addEventListener("focus", focus);
        return () => {
            stopped = true;
            clearInterval(timer);
            window.removeEventListener("focus", focus);
        };
    }, [active]);
    return allowed;
}

export function openNotificationSettings(): Promise<void> {
    return invoke("open_notification_settings");
}
