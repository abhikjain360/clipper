import type { KitchenRecipeSummary } from "@clipper/mobile-bridge";
import type { AppState } from "@clipper/shared";
import { Search } from "lucide-react-native";
import { useCallback, useState } from "react";
import { H2, Input, ScrollView, Text, XStack, YStack } from "tamagui";
import { colors, GymCard, Muted } from "../gym/GymUi";
import { blockTime, deviceZone, kitchen, localTime, useKitchenView } from "./kitchenClient";
import { LoadStatus } from "./KitchenUi";

export function RecipeList({
  state,
  onOpen,
  onError,
}: {
  state: AppState;
  onOpen: (id: string) => void;
  onError: (error: string | null) => void;
}) {
  const [search, setSearch] = useState("");
  const zone = deviceZone();
  const query = useCallback(() => kitchen().kitchenRecipes(search, zone), [search, zone]);
  const { view, loading, failed, reload } = useKitchenView(query, state, onError);

  return (
    <YStack flex={1} gap="$3">
      <H2 size="$6">Recipes</H2>
      <XStack items="center" gap="$2">
        <Search size={20} color={colors.muted} />
        <Input
          flex={1}
          aria-label="Search recipes"
          placeholder="Search recipes"
          value={search}
          onChangeText={setSearch}
          autoCorrect={false}
        />
      </XStack>
      <LoadStatus loading={loading} failed={failed} onRetry={reload} />
      <ScrollView flex={1} keyboardShouldPersistTaps="always">
        <YStack gap="$3" pb="$8">
          {view && view.openSessions.length > 0 && (
            <GymCard highlighted>
              <YStack gap="$2">
                <Text fontWeight="600">Cooking now</Text>
                {view.openSessions.map((session) => (
                  <YStack
                    key={session.sessionId}
                    gap="$1"
                    py="$2"
                    onPress={() => onOpen(session.recipeId)}
                    pressStyle={{ bg: palette.raised }}
                    accessibilityRole="button"
                    aria-label={`Open ${session.title}`}
                  >
                    <Text fontWeight="600" fontSize={18}>
                      {session.title}
                    </Text>
                    <Muted>
                      {`Started ${localTime(session.startedAtMillis)} · Revision ${session.recipeRevision}${session.deleted ? " · Recipe deleted" : ""}`}
                    </Muted>
                  </YStack>
                ))}
              </YStack>
            </GymCard>
          )}
          {search.trim() === "" && view?.nextBlock && (
            <GymCard highlighted>
              <YStack gap="$2">
                <Text fontWeight="600">
                  {`Next cooking: ${view.nextBlock.title}, ${blockTime(view.nextBlock.startMillis)}`}
                </Text>
                {view.nextBlockRecipes.map((recipe) => (
                  <RecipeRow key={recipe.id} recipe={recipe} onOpen={onOpen} />
                ))}
              </YStack>
            </GymCard>
          )}
          {view?.recipes.length === 0 && !loading && (
            <Muted>{search.trim() ? "No matching recipes" : "No recipes yet"}</Muted>
          )}
          {view?.recipes.map((recipe) => (
            <GymCard key={recipe.id}>
              <RecipeRow recipe={recipe} onOpen={onOpen} />
            </GymCard>
          ))}
        </YStack>
      </ScrollView>
    </YStack>
  );
}

function RecipeRow({
  recipe,
  onOpen,
}: {
  recipe: KitchenRecipeSummary;
  onOpen: (id: string) => void;
}) {
  return (
    <YStack
      gap="$1"
      py="$2"
      onPress={() => onOpen(recipe.id)}
      pressStyle={{ bg: palette.raised }}
      accessibilityRole="button"
      aria-label={`Open ${recipe.title}`}
    >
      <XStack items="center" gap="$2" flexWrap="wrap">
        <Text fontWeight="600" fontSize={18} shrink={1}>
          {recipe.title}
        </Text>
        {recipe.cooking && (
          <Text
            color={colors.good}
            bg={statusSurfaces.success}
            px="$2"
            py="$1"
            rounded="$2"
            fontSize={12}
          >
            Cooking
          </Text>
        )}
      </XStack>
      <Muted>{recipe.summary}</Muted>
      <Muted>{[recipe.cuisine, ...recipe.tags].filter(Boolean).join(" · ")}</Muted>
      <Muted>{`${recipe.totalMinutes} min total`}</Muted>
      {recipe.nextBlock && (
        <Text color={colors.accent}>
          {`${recipe.nextBlock.title} · ${blockTime(recipe.nextBlock.startMillis)}`}
        </Text>
      )}
    </YStack>
  );
}
import { palette, statusSurfaces } from "@clipper/shared";
