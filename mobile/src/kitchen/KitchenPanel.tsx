import type { AppState } from "@clipper/shared";
import { useState } from "react";
import { Button, XStack, YStack } from "tamagui";
import { Pantry } from "./Pantry";
import { RecipeHistory } from "./RecipeHistory";
import { RecipeList } from "./RecipeList";
import { RecipePage } from "./RecipePage";

export function KitchenPanel({
  state,
  openRecipeId,
  onOpenRecipe,
  onError,
}: {
  state: AppState;
  openRecipeId: string | null;
  onOpenRecipe: (id: string | null) => void;
  onError: (error: string | null) => void;
}) {
  const [section, setSection] = useState<"recipes" | "pantry">("recipes");

  return (
    <YStack flex={1} gap="$3" pt="$3">
      <XStack gap="$2">
        <Button
          size="$3"
          theme={openRecipeId !== null || section === "recipes" ? "blue" : undefined}
          onPress={() => {
            onOpenRecipe(null);
            setSection("recipes");
          }}
        >
          Recipes
        </Button>
        <Button
          size="$3"
          theme={openRecipeId === null && section === "pantry" ? "blue" : undefined}
          onPress={() => {
            onOpenRecipe(null);
            setSection("pantry");
          }}
        >
          Pantry
        </Button>
      </XStack>
      {openRecipeId !== null ? (
        <RecipeScreen
          key={openRecipeId}
          recipeId={openRecipeId}
          state={state}
          onBack={() => {
            onOpenRecipe(null);
            setSection("recipes");
          }}
          onError={onError}
        />
      ) : section === "recipes" ? (
        <RecipeList state={state} onOpen={onOpenRecipe} onError={onError} />
      ) : (
        <Pantry state={state} onError={onError} />
      )}
    </YStack>
  );
}

function RecipeScreen({
  recipeId,
  state,
  onBack,
  onError,
}: {
  recipeId: string;
  state: AppState;
  onBack: () => void;
  onError: (error: string | null) => void;
}) {
  const [history, setHistory] = useState(false);
  const [revision, setRevision] = useState<bigint | undefined>(undefined);

  if (history) {
    return (
      <RecipeHistory
        recipeId={recipeId}
        state={state}
        onBack={() => setHistory(false)}
        onOpen={(value) => {
          setRevision(value);
          setHistory(false);
        }}
        onError={onError}
      />
    );
  }

  return (
    <RecipePage
      key={revision === undefined ? "current" : String(revision)}
      recipeId={recipeId}
      revision={revision}
      state={state}
      onBack={
        revision === undefined
          ? onBack
          : () => {
              setRevision(undefined);
              setHistory(true);
            }
      }
      onHistory={() => setHistory(true)}
      onError={onError}
    />
  );
}
