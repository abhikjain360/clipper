import { useEffect, useId, useState, type ReactNode } from "react";
import { createPortal } from "react-dom";

export function EventHover({
    title,
    detail,
    children,
}: {
    title: string;
    detail: string;
    children: ReactNode;
}) {
    const id = useId();
    const [point, setPoint] = useState<{ left: number; top: number } | null>(null);
    useEffect(() => {
        if (!point) return;
        const close = () => setPoint(null);
        const key = (event: KeyboardEvent) => {
            if (event.key === "Escape") close();
        };
        window.addEventListener("scroll", close, true);
        window.addEventListener("resize", close);
        window.addEventListener("keydown", key);
        return () => {
            window.removeEventListener("scroll", close, true);
            window.removeEventListener("resize", close);
            window.removeEventListener("keydown", key);
        };
    }, [point]);
    function show(element: HTMLElement) {
        const rect = element.firstElementChild?.getBoundingClientRect();
        if (!rect) return;
        setPoint({
            left: Math.max(8, Math.min(rect.left, window.innerWidth - 328)),
            top:
                rect.bottom + 150 < window.innerHeight
                    ? rect.bottom + 8
                    : Math.max(8, rect.top - 140),
        });
    }
    return (
        <span
            style={{ display: "contents" }}
            onMouseEnter={(e) => show(e.currentTarget)}
            onMouseLeave={() => setPoint(null)}
            onFocus={(e) => show(e.currentTarget)}
            onBlur={() => setPoint(null)}
            aria-describedby={point ? id : undefined}
        >
            {children}
            {point &&
                createPortal(
                    <div id={id} role="tooltip" className="event-hover" style={point}>
                        <strong>{title || "Untitled"}</strong>
                        <div>{detail}</div>
                    </div>,
                    document.body,
                )}
        </span>
    );
}
