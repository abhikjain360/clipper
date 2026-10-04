import { useEffect, useState } from "react";
import { Button, Spinner, XStack, YStack } from "tamagui";
import { formatBackendError } from "../backend";
import { BodyWeight } from "./BodyWeight";
import { Fatigue } from "./Fatigue";
import { gym } from "./gymClient";
import { History } from "./History";
import { Library } from "./Library";
import { LiveSession } from "./LiveSession";
import { Progress } from "./Progress";

type Section = "session" | "history" | "weight" | "progress" | "fatigue" | "library";

const sections: { value: Section; label: string }[] = [
  { value: "session", label: "Workout" },
  { value: "history", label: "History" },
  { value: "weight", label: "Weight" },
  { value: "progress", label: "Progress" },
  { value: "fatigue", label: "Fatigue" },
  { value: "library", label: "Library" },
];

export function GymPanel({ onError }: { onError: (error: string | null) => void }) {
  const [section, setSection] = useState<Section>("session");
  const [ready, setReady] = useState(false);

  useEffect(() => {
    let cancelled = false;
    void (async () => {
      try {
        await gym().gymSeedStarterLibrary();
      } catch (caught) {
        if (!cancelled) onError(formatBackendError(caught));
      } finally {
        if (!cancelled) setReady(true);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, [onError]);

  return (
    <YStack flex={1} gap="$3" pt="$3">
      <XStack gap="$2" flexWrap="wrap">
        {sections.map(({ value, label }) => (
          <Button
            key={value}
            size="$3"
            theme={section === value ? "blue" : undefined}
            onPress={() => setSection(value)}
          >
            {label}
          </Button>
        ))}
      </XStack>
      {!ready ? (
        <YStack flex={1} items="center" justify="center">
          <Spinner />
        </YStack>
      ) : (
        <>
          {section === "session" && <LiveSession onError={onError} />}
          {section === "history" && <History onError={onError} />}
          {section === "weight" && <BodyWeight onError={onError} />}
          {section === "progress" && <Progress onError={onError} />}
          {section === "fatigue" && <Fatigue onError={onError} />}
          {section === "library" && <Library onError={onError} />}
        </>
      )}
    </YStack>
  );
}

export async function hasOpenGymSession(): Promise<boolean> {
  try {
    return (await gym().gymOpenSession()) !== undefined;
  } catch {
    return false;
  }
}
