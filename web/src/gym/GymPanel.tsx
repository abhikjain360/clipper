import { useEffect, useState } from "react";
import { H2, Paragraph, Spinner, XStack, YStack } from "tamagui";
import { Button } from "../tamagui.config";
import { palette, type GymBackend } from "@clipper/shared";
import { clipperBackend, formatBackendError } from "../backend";
import { BodyWeight } from "./BodyWeight";
import { Fatigue } from "./Fatigue";
import type { ErrorHandler } from "./GymUi";
import { History } from "./History";
import { Library } from "./Library";
import { Progress } from "./Progress";
import { Workout } from "./Workout";

type Section = "workout" | "history" | "weight" | "progress" | "fatigue" | "library";

const sections: { value: Section; label: string }[] = [
    { value: "workout", label: "Workout" },
    { value: "history", label: "History" },
    { value: "weight", label: "Weight" },
    { value: "progress", label: "Progress" },
    { value: "fatigue", label: "Fatigue" },
    { value: "library", label: "Library" },
];

export function GymPanel({ state, onError }: { state: unknown; onError: ErrorHandler }) {
    const [backend, setBackend] = useState<GymBackend | null | undefined>(undefined);
    const [section, setSection] = useState<Section>("workout");
    const [error, setError] = useState<string | null>(null);
    const [version, setVersion] = useState(0);
    const [libraryWritten, setLibraryWritten] = useState(0);

    useEffect(() => {
        let cancelled = false;
        setError(null);
        void clipperBackend()
            .then((result) => {
                if (!cancelled) setBackend(result.gym ?? null);
            })
            .catch((caught: unknown) => {
                if (cancelled) return;
                const message = formatBackendError(caught);
                setError(message);
                onError(message);
            });
        return () => {
            cancelled = true;
        };
    }, [onError, version]);

    useEffect(() => {
        if (!backend) return;
        const gym = backend;
        let cancelled = false;
        let retry: ReturnType<typeof setTimeout> | undefined;
        async function seed() {
            try {
                const outcome = await gym.seedStarterLibrary();
                if (cancelled) return;
                if (outcome === "written") setLibraryWritten((count) => count + 1);
                if (outcome === "waiting_for_download") retry = setTimeout(() => void seed(), 2000);
            } catch (caught) {
                if (!cancelled) onError(formatBackendError(caught));
            }
        }
        void seed();
        return () => {
            cancelled = true;
            if (retry) clearTimeout(retry);
        };
    }, [backend, onError]);

    if (error)
        return (
            <YStack gap="$2">
                <Paragraph role="alert" color={palette.danger}>
                    {error}
                </Paragraph>
                <Button onPress={() => setVersion((current) => current + 1)}>Retry</Button>
            </YStack>
        );
    if (backend === undefined) return <Spinner />;
    if (backend === null)
        return <Paragraph>Gym needs the Clipper app on a phone or Mac.</Paragraph>;
    return (
        <YStack gap="$4" maxW={1200} width="100%" self="center">
            <H2>Gym</H2>
            <XStack gap="$2" flexWrap="wrap" role="group" aria-label="Gym sections">
                {sections.map(({ value, label }) => (
                    <Button
                        key={value}
                        aria-pressed={section === value}
                        theme={section === value ? "blue" : undefined}
                        onPress={() => setSection(value)}
                    >
                        {label}
                    </Button>
                ))}
            </XStack>
            <YStack key={libraryWritten}>
                {section === "workout" && (
                    <Workout backend={backend} state={state} onError={onError} />
                )}
                {section === "history" && (
                    <History backend={backend} state={state} onError={onError} />
                )}
                {section === "weight" && (
                    <BodyWeight backend={backend} state={state} onError={onError} />
                )}
                {section === "progress" && (
                    <Progress backend={backend} state={state} onError={onError} />
                )}
                {section === "fatigue" && (
                    <Fatigue backend={backend} state={state} onError={onError} />
                )}
                {section === "library" && (
                    <Library backend={backend} state={state} onError={onError} />
                )}
            </YStack>
        </YStack>
    );
}
