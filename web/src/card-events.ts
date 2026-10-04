type CardEvent = { target: unknown; currentTarget: unknown };

export function cardEvents(open: () => void) {
    return {
        onClick(event: CardEvent) {
            const target = event.target as HTMLElement | null;
            const action = target?.closest?.("button, a, input, select, textarea, [role=button]");
            if (action && action !== event.currentTarget) return;
            open();
        },
        onKeyDown(event: CardEvent & { key?: string; preventDefault: () => void }) {
            if (event.key === "Enter" && event.target === event.currentTarget) {
                event.preventDefault();
                open();
            }
        },
    };
}
