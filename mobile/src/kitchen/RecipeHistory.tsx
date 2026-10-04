import { Button } from "../tamagui.config";
import type { AppState } from "@clipper/shared";
import { ArrowLeft } from "lucide-react-native";
import { useCallback } from "react";
import { H2, ScrollView, Text, XStack, YStack } from "tamagui";
import { colors, GymCard, Muted } from "../gym/GymUi";
import { kitchen, useKitchenView } from "./kitchenClient";
import { LoadStatus, useBack } from "./KitchenUi";

export function RecipeHistory({
  recipeId,
  state,
  onBack,
  onOpen,
  onError,
}: {
  recipeId: string;
  state: AppState;
  onBack: () => void;
  onOpen: (revision: bigint) => void;
  onError: (error: string | null) => void;
}) {
  const query = useCallback(() => kitchen().kitchenRecipeHistory(recipeId), [recipeId]);
  const { view, loading, failed, reload } = useKitchenView(query, state, onError);
  useBack(onBack);

  return (
    <YStack flex={1} gap="$3">
      <XStack items="center" gap="$2">
        <Button
          size="$3"
          icon={<ArrowLeft size={16} />}
          onPress={onBack}
          aria-label="Back to recipe"
        />
        <H2 size="$6">Recipe history</H2>
      </XStack>
      <Muted>History needs a server connection.</Muted>
      <LoadStatus loading={loading} failed={failed} onRetry={reload} />
      <ScrollView flex={1}>
        <YStack gap="$2" pb="$8">
          {view?.length === 0 && !loading && <Muted>No revisions</Muted>}
          {view?.map((entry) => (
            <GymCard key={String(entry.revision)}>
              <YStack gap="$2">
                <Text fontWeight="600">{`Revision ${entry.revision}`}</Text>
                <Muted>{new Date(entry.writtenAt).toLocaleString()}</Muted>
                <Text color={colors.muted} accessibilityLabel={`Writing device ${entry.deviceId}`}>
                  {`Device ${entry.deviceId.slice(0, 8)}`}
                </Text>
                {entry.deleted ? (
                  <Text color={colors.bad}>Deleted</Text>
                ) : (
                  <Button size="$3" onPress={() => onOpen(entry.revision)}>
                    View revision
                  </Button>
                )}
              </YStack>
            </GymCard>
          ))}
        </YStack>
      </ScrollView>
    </YStack>
  );
}
