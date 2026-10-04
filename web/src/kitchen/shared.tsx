import { Button } from "../tamagui.config";
import { palette } from "@clipper/shared";
import { cloneElement, isValidElement, useEffect, useId, useState, type ReactNode } from "react";
import { Card, Dialog, Label, Paragraph, Spinner, YStack } from "tamagui";
import { formatBackendError } from "../backend";

export type ErrorHandler = (error: string | null) => void;

export function useKitchenData<T>(
    load: () => Promise<T>,
    state: unknown,
    version: number,
    onError: ErrorHandler,
) {
    const [value, setValue] = useState<T | null>(null);
    const [loading, setLoading] = useState(true);
    const [failed, setFailed] = useState(false);
    const [error, setError] = useState<string | null>(null);
    useEffect(() => {
        let cancelled = false;
        setLoading(true);
        setFailed(false);
        setError(null);
        void load()
            .then((result) => {
                if (!cancelled) setValue(result);
            })
            .catch((caught: unknown) => {
                if (!cancelled) {
                    setValue(null);
                    setFailed(true);
                    const message = formatBackendError(caught);
                    setError(message);
                    onError(message);
                }
            })
            .finally(() => {
                if (!cancelled) setLoading(false);
            });
        return () => {
            cancelled = true;
        };
    }, [load, state, version, onError]);
    return { value, loading, failed, error };
}

export function KitchenCard({ children }: { children: ReactNode }) {
    return (
        <Card bg={palette.cardFill} p="$3" gap="$3">
            {children}
        </Card>
    );
}

export function Field({ label, children }: { label: string; children: ReactNode }) {
    const id = useId();
    return (
        <YStack gap="$1">
            <Label htmlFor={id} fontSize={12} color={palette.secondary}>
                {label}
            </Label>
            {isValidElement<{ id?: string }>(children) ? cloneElement(children, { id }) : children}
        </YStack>
    );
}

export function Loading({
    loading,
    failed,
    error,
}: {
    loading: boolean;
    failed: boolean;
    error?: string | null;
}) {
    return (
        <>
            {loading && <Spinner size="small" />}
            {failed && (
                <Paragraph role="status" color={palette.danger}>
                    {error ?? "Could not load Kitchen data. Try Refresh."}
                </Paragraph>
            )}
        </>
    );
}

export function blockTime(millis: number): string {
    return new Date(millis).toLocaleString(undefined, {
        weekday: "long",
        month: "short",
        day: "numeric",
        hour: "2-digit",
        minute: "2-digit",
    });
}

export function localTime(millis: number | string): string {
    return new Date(millis).toLocaleString();
}

export function kitchenZone(): string {
    return Intl.DateTimeFormat().resolvedOptions().timeZone;
}

export function KitchenDialog({
    open,
    title,
    description,
    onClose,
    children,
    busy = false,
}: {
    open: boolean;
    title: string;
    description: string;
    onClose: () => void;
    children: ReactNode;
    busy?: boolean;
}) {
    return (
        <Dialog
            modal
            open={open}
            onOpenChange={(next) => {
                if (!next && !busy) onClose();
            }}
        >
            <Dialog.Portal>
                <Dialog.Overlay key="overlay" bg="rgba(0,0,0,0.65)" />
                <Dialog.Content
                    key="content"
                    width="90vw"
                    maxW={700}
                    maxH="90vh"
                    p="$3"
                    gap="$3"
                    style={{ overflowY: "auto" }}
                >
                    <Dialog.Title fontSize={24}>{title}</Dialog.Title>
                    <Dialog.Description>{description}</Dialog.Description>
                    {children}
                    <Button disabled={busy} onPress={onClose}>
                        Close
                    </Button>
                </Dialog.Content>
            </Dialog.Portal>
        </Dialog>
    );
}
