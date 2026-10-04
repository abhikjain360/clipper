export function formatBackendError(error: unknown): string {
    if (error instanceof Error) return error.message;

    if (typeof error === "object" && error !== null) {
        const message = (error as { message?: unknown }).message;
        if (typeof message === "string" && message.length > 0) return message;

        try {
            return JSON.stringify(error);
        } catch {
            return String(error);
        }
    }

    return String(error);
}
