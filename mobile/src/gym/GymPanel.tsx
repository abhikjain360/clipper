import { Button } from "../tamagui.config";
import { useEffect, useState } from "react";
import { XStack, YStack } from "tamagui";
import { GymStarterLibrary } from "@clipper/mobile-bridge";
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
  const [libraryWritten, setLibraryWritten] = useState(0);

  useEffect(() => {
    let cancelled = false;
    let retry: ReturnType<typeof setTimeout> | undefined;
    async function seed() {
      try {
        const outcome = await gym().gymSeedStarterLibrary();
        if (cancelled) return;
        if (outcome === GymStarterLibrary.Written) setLibraryWritten((count) => count + 1);
        if (outcome === GymStarterLibrary.WaitingForDownload)
          retry = setTimeout(() => void seed(), 2000);
      } catch (caught) {
        if (!cancelled) onError(formatBackendError(caught));
      }
    }
    void seed();
    return () => {
      cancelled = true;
      if (retry) clearTimeout(retry);
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
      <YStack key={libraryWritten} flex={1}>
        {section === "session" && <LiveSession onError={onError} />}
        {section === "history" && <History onError={onError} />}
        {section === "weight" && <BodyWeight onError={onError} />}
        {section === "progress" && <Progress onError={onError} />}
        {section === "fatigue" && <Fatigue onError={onError} />}
        {section === "library" && <Library onError={onError} />}
      </YStack>
    </YStack>
  );
}
