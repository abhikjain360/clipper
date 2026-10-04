import {
    createContext,
    useCallback,
    useContext,
    useEffect,
    useId,
    useLayoutEffect,
    useState,
} from "react";

export function useCalendarHover(refresh: unknown) {
    const [hovered, setHovered] = useState<{ key: string; refresh: unknown } | null>(null);
    const close = useCallback(() => setHovered(null), []);
    const enter = useCallback((key: string) => setHovered({ key, refresh }), [refresh]);
    const leave = useCallback((key: string) => {
        setHovered((current) => (current?.key === key ? null : current));
    }, []);

    useLayoutEffect(close, [close, refresh]);
    useEffect(() => {
        const hide = () => {
            if (document.hidden) close();
        };
        document.addEventListener("scroll", close, true);
        document.addEventListener("visibilitychange", hide);
        window.addEventListener("blur", close);
        return () => {
            document.removeEventListener("scroll", close, true);
            document.removeEventListener("visibilitychange", hide);
            window.removeEventListener("blur", close);
        };
    }, [close]);

    return {
        key: hovered && hovered.refresh === refresh ? hovered.key : null,
        enter,
        leave,
        close,
    };
}

export const CalendarHoverContext = createContext<ReturnType<typeof useCalendarHover> | null>(null);

export function useEventHover() {
    const hover = useContext(CalendarHoverContext);
    const key = useId();
    if (!hover) throw new Error("EventHover requires calendar hover state");
    const leave = hover.leave;
    useEffect(() => () => leave(key), [leave, key]);

    return {
        open: hover.key === key,
        onOpenChange: (open: boolean) => {
            if (!open) leave(key);
        },
        onPointerEnter: (event: { nativeEvent: { pointerType?: string } }) => {
            if (event.nativeEvent.pointerType !== "touch") hover.enter(key);
        },
        onPointerLeave: () => leave(key),
        onPointerCancel: () => leave(key),
    };
}
