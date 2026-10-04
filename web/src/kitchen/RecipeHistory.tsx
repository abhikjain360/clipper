import { Button } from "../tamagui.config";
import { palette } from "@clipper/shared";
import { useCallback, useState } from "react";
import { Paragraph, Text, XStack, YStack } from "tamagui";
import type { KitchenBackend } from "@clipper/shared";
import { KitchenCard, Loading, localTime, useKitchenData, type ErrorHandler } from "./shared";

export function RecipeHistory({
    id,
    backend,
    state,
    onError,
    onRevision,
}: {
    id: string;
    backend: KitchenBackend;
    state: unknown;
    onError: ErrorHandler;
    onRevision: (revision: number) => void;
}) {
    const [version, setVersion] = useState(0);
    const load = useCallback(() => backend.recipeHistory(id), [backend, id]);
    const { value, loading, failed, error } = useKitchenData(load, state, version, onError);
    return (
        <YStack gap="$3">
            <Button
                self="flex-start"
                disabled={loading}
                onPress={() => setVersion((current) => current + 1)}
            >
                Refresh
            </Button>
            <Loading loading={loading} failed={failed} error={error} />
            {value?.map((entry) => (
                <KitchenCard key={entry.revision}>
                    <XStack gap="$2" items="center" flexWrap="wrap">
                        {entry.deleted ? (
                            <Text>Revision {entry.revision} · Deleted</Text>
                        ) : (
                            <Button onPress={() => onRevision(entry.revision)}>
                                Revision {entry.revision}
                            </Button>
                        )}
                        <Text>{localTime(entry.written_at)}</Text>
                        <span title={entry.device_id}>
                            <Text color={palette.secondary}>
                                Device {entry.device_id.slice(0, 8)}
                            </Text>
                        </span>
                    </XStack>
                </KitchenCard>
            ))}
            {!loading && !failed && value?.length === 0 && <Paragraph>No revisions yet.</Paragraph>}
        </YStack>
    );
}
