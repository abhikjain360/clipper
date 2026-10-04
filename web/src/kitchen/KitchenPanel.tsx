import { useEffect, useState } from "react";
import { Button, H2, Paragraph, Spinner, XStack, YStack } from "tamagui";
import type { KitchenBackend } from "@clipper/shared";
import { clipperBackend, formatBackendError } from "../backend";
import { Pantry } from "./Pantry";
import { RecipeList } from "./RecipeList";
import { RecipePage } from "./RecipePage";
import type { ErrorHandler } from "./shared";

export function KitchenPanel({
    id,
    state,
    onError,
}: {
    id?: string;
    state: unknown;
    onError: ErrorHandler;
}) {
    const [backend, setBackend] = useState<KitchenBackend | null | undefined>(undefined);
    const [section, setSection] = useState<"recipes" | "pantry">("recipes");
    const [error, setError] = useState<string | null>(null);
    const [version, setVersion] = useState(0);
    useEffect(() => {
        let cancelled = false;
        setError(null);
        void clipperBackend()
            .then((result) => {
                if (!cancelled) setBackend(result.kitchen ?? null);
            })
            .catch((caught: unknown) => {
                if (!cancelled) {
                    const message = formatBackendError(caught);
                    setError(message);
                    onError(message);
                }
            });
        return () => {
            cancelled = true;
        };
    }, [onError, version]);

    if (error)
        return (
            <YStack gap="$2">
                <Paragraph role="alert" color="#ff7b7b">
                    {error}
                </Paragraph>
                <Button onPress={() => setVersion((current) => current + 1)}>Retry</Button>
            </YStack>
        );
    if (backend === undefined) return <Spinner />;
    if (backend === null)
        return <Paragraph>Kitchen needs the Clipper app on a phone or Mac.</Paragraph>;
    if (id)
        return <RecipePage key={id} id={id} backend={backend} state={state} onError={onError} />;
    return (
        <YStack gap="$3" maxW={1000} width="100%" self="center">
            <H2>Kitchen</H2>
            <XStack gap="$2" role="group" aria-label="Kitchen sections">
                <Button
                    theme={section === "recipes" ? "blue" : undefined}
                    onPress={() => setSection("recipes")}
                >
                    Recipes
                </Button>
                <Button
                    theme={section === "pantry" ? "blue" : undefined}
                    onPress={() => setSection("pantry")}
                >
                    Pantry
                </Button>
            </XStack>
            {section === "recipes" ? (
                <RecipeList backend={backend} state={state} onError={onError} />
            ) : (
                <Pantry backend={backend} state={state} onError={onError} />
            )}
        </YStack>
    );
}
