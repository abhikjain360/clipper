import { Button } from "./Button";
import { useCallback, useEffect, useState } from "react";
import { KeyboardAvoidingView } from "react-native";
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
import { GymError } from "./errors";

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
  const [error, setError] = useState<string | null>(null);
  const reportError = useCallback(
    (message: string | null) => {
      setError(message);
      onError(message);
    },
    [onError],
  );
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
    <GymError.Provider value={error}>
      <YStack flex={1} gap="$3" pt="$3">
        <XStack gap="$2" flexWrap="wrap">
          {sections.map(({ value, label }) => (
            <Button
              key={value}
              size="$3"
              selected={section === value}
              onPress={() => setSection(value)}
            >
              {label}
            </Button>
          ))}
        </XStack>
        <KeyboardAvoidingView key={libraryWritten} style={{ flex: 1 }} behavior="padding">
          {section === "session" && <LiveSession onError={reportError} />}
          {section === "history" && <History onError={reportError} />}
          {section === "weight" && <BodyWeight onError={reportError} />}
          {section === "progress" && <Progress onError={reportError} />}
          {section === "fatigue" && <Fatigue onError={reportError} />}
          {section === "library" && <Library onError={reportError} />}
        </KeyboardAvoidingView>
      </YStack>
    </GymError.Provider>
  );
}
