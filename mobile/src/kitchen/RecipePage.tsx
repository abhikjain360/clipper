import { KitchenSessionChange, type KitchenIngredient } from "@clipper/mobile-bridge";
import type { AppState } from "@clipper/shared";
import { ArrowLeft, Minus, Plus } from "lucide-react-native";
import { useCallback, useEffect, useState } from "react";
import { Alert } from "react-native";
import { Button, H2, Text, ScrollView, XStack, YStack } from "tamagui";
import { keepScreenOn } from "../../modules/clipper-alarm";
import { formatBackendError } from "../backend";
import { colors, GymCard, Muted, SheetModal } from "../gym/GymUi";
import {
  blockTime,
  deviceZone,
  kitchen,
  localTime,
  useKitchenChange,
  useKitchenView,
} from "./kitchenClient";
import { LoadStatus, NotesInput, Tick, useBack } from "./KitchenUi";
import { StepTimer } from "./StepTimer";

export function RecipePage({
  recipeId,
  revision,
  state,
  onBack,
  onHistory,
  onError,
}: {
  recipeId: string;
  revision?: bigint;
  state: AppState;
  onBack: () => void;
  onHistory: () => void;
  onError: (error: string | null) => void;
}) {
  const [servings, setServings] = useState<number | undefined>(undefined);
  const [finishing, setFinishing] = useState(false);
  const [notes, setNotes] = useState("");
  const zone = deviceZone();
  const query = useCallback(
    () =>
      revision === undefined
        ? kitchen().kitchenRecipe(recipeId, servings, zone)
        : kitchen().kitchenRecipeRevision(recipeId, revision, servings),
    [recipeId, revision, servings, zone],
  );
  const { view, loading, failed, reload } = useKitchenView(query, state, onError);
  const { busy, error, run } = useKitchenChange(reload, onError);
  const disabled = busy || loading || failed;
  useBack(onBack);

  useEffect(() => {
    try {
      keepScreenOn(true);
    } catch (caught) {
      onError(formatBackendError(caught));
    }
    return () => {
      try {
        keepScreenOn(false);
      } catch (caught) {
        onError(formatBackendError(caught));
      }
    };
  }, [onError]);

  function change(value: KitchenSessionChange) {
    if (!view || view.readOnly || disabled) return;
    void run(() => kitchen().kitchenChangeSession(recipeId, view.revision, view.servings, value));
  }

  function changeServings(value: number) {
    if (!view || disabled) return;
    if (view.session) change(new KitchenSessionChange.Servings({ servings: value }));
    else setServings(value);
  }

  async function finish() {
    if (!view || view.readOnly || disabled) return;
    const saved = await run(() =>
      kitchen().kitchenChangeSession(
        recipeId,
        view.revision,
        view.servings,
        new KitchenSessionChange.Finish({ notes: notes.trim() || undefined }),
      ),
    );
    if (saved) {
      setFinishing(false);
      setNotes("");
      if (view.deleted) onBack();
    }
  }

  async function discardSession() {
    if (!view || view.readOnly || disabled) return;
    const saved = await run(() =>
      kitchen().kitchenChangeSession(
        recipeId,
        view.revision,
        view.servings,
        new KitchenSessionChange.Discard(),
      ),
    );
    if (saved && view.deleted) onBack();
  }

  function discard() {
    Alert.alert("Discard cooking session?", "The checklist, timers and session are deleted.", [
      { text: "Cancel", style: "cancel" },
      {
        text: "Discard",
        style: "destructive",
        onPress: () => void discardSession(),
      },
    ]);
  }

  const editable = view !== null && !view.readOnly && !view.deleted;
  return (
    <YStack flex={1} gap="$3">
      <XStack items="center" gap="$2">
        <Button
          size="$3"
          aria-label={revision === undefined ? "Back to recipes" : "Back to history"}
          icon={<ArrowLeft size={16} />}
          onPress={onBack}
        >
          Back
        </Button>
        {revision === undefined && (
          <Button size="$3" onPress={onHistory}>
            History
          </Button>
        )}
      </XStack>
      {view?.readOnly && <H2 size="$5">{`Revision ${view.revision} (read-only)`}</H2>}
      <LoadStatus loading={loading} failed={failed} onRetry={reload} />
      {view && (
        <ScrollView flex={1} keyboardShouldPersistTaps="always">
          <YStack gap="$3" pb="$8">
            {view.deleted && (
              <Text color={colors.warm}>
                This recipe was deleted. Finish or discard this cooking session.
              </Text>
            )}
            {view.otherSessions.map((other) => (
              <Text key={other.id} color={colors.warm}>
                {`Another cooking session from revision ${other.recipeRevision}, started ${localTime(other.startedAtMillis)}, is open. It shows here once this one is finished or discarded.`}
              </Text>
            ))}
            {view.newerRevision !== undefined && (
              <Text color={colors.warm}>
                {`This session cooks from revision ${view.revision}. Revision ${view.newerRevision} is newer and applies from the next cook.`}
              </Text>
            )}
            <YStack gap="$2">
              <H2 size="$6">{view.title}</H2>
              <Text>{view.summary}</Text>
              <Muted>{[view.cuisine, ...view.tags].filter(Boolean).join(" · ")}</Muted>
              <Muted>{`Active ${view.activeMinutes} min · Total ${view.totalMinutes} min`}</Muted>
            </YStack>
            <XStack items="center" gap="$3" justify="space-between">
              <Text>Servings</Text>
              <XStack items="center" gap="$3">
                <Button
                  size="$4"
                  aria-label="Fewer servings"
                  icon={<Minus size={18} />}
                  disabled={disabled || view.servings <= 1}
                  onPress={() => changeServings(view.servings - 1)}
                />
                <Text fontSize={20}>{String(view.servings)}</Text>
                <Button
                  size="$4"
                  aria-label="More servings"
                  icon={<Plus size={18} />}
                  disabled={disabled}
                  onPress={() => changeServings(view.servings + 1)}
                />
              </XStack>
            </XStack>
            {!view.readOnly &&
              (view.session ? (
                <YStack gap="$2">
                  <Muted>{`Cooking · Started ${localTime(view.session.startedAtMillis)}`}</Muted>
                  <XStack gap="$2">
                    <Button
                      theme="green"
                      disabled={disabled}
                      onPress={() => {
                        setNotes("");
                        setFinishing(true);
                      }}
                    >
                      Finish
                    </Button>
                    <Button disabled={disabled} onPress={discard}>
                      Discard
                    </Button>
                  </XStack>
                </YStack>
              ) : (
                <Button
                  theme="blue"
                  size="$5"
                  disabled={disabled}
                  onPress={() => change(new KitchenSessionChange.Start())}
                >
                  Start cooking
                </Button>
              ))}
            {view.nutrition && (
              <GymCard>
                <YStack gap="$1">
                  <Text fontWeight="600">Nutrition per serving</Text>
                  <Muted>{`${view.nutrition.calories} kcal · Protein ${view.nutrition.proteinGrams} g`}</Muted>
                  <Muted>{`Carbs ${view.nutrition.carbsGrams} g · Fat ${view.nutrition.fatGrams} g`}</Muted>
                  {view.nutrition.fiberGrams !== undefined && (
                    <Muted>{`Fiber ${view.nutrition.fiberGrams} g`}</Muted>
                  )}
                </YStack>
              </GymCard>
            )}
            <H2 size="$5">Ingredients</H2>
            {view.groups.map((group, index) => (
              <GymCard key={index}>
                <YStack gap="$2">
                  {group.name && <Text fontWeight="600">{group.name}</Text>}
                  {group.ingredients.map((ingredient) => (
                    <XStack key={ingredient.id} items="center" gap="$2">
                      {editable && (
                        <Tick
                          checked={ingredient.gathered}
                          label={`Gather ${ingredient.name}`}
                          disabled={disabled}
                          onPress={() =>
                            change(
                              new KitchenSessionChange.Gathered({
                                ingredient: ingredient.id,
                                gathered: !ingredient.gathered,
                              }),
                            )
                          }
                        />
                      )}
                      <Ingredient ingredient={ingredient} />
                    </XStack>
                  ))}
                </YStack>
              </GymCard>
            ))}
            <H2 size="$5">Shopping list</H2>
            {view.shopping.length === 0 ? (
              <Muted>Nothing to buy</Muted>
            ) : (
              view.shopping.map((ingredient) => (
                <Ingredient key={ingredient.id} ingredient={ingredient} />
              ))
            )}
            <H2 size="$5">Equipment</H2>
            {view.equipment.length === 0 && <Muted>No equipment listed</Muted>}
            {view.equipment.map((name, index) => (
              <Text key={index}>{name}</Text>
            ))}
            <H2 size="$5">Steps</H2>
            {view.steps.map((step) => (
              <GymCard key={step.index}>
                <YStack gap="$3">
                  <XStack items="flex-start" gap="$2">
                    {editable && (
                      <Tick
                        checked={step.done}
                        label={`Step ${step.index + 1} done`}
                        disabled={disabled}
                        onPress={() =>
                          change(
                            new KitchenSessionChange.StepDone({
                              step: step.index,
                              done: !step.done,
                            }),
                          )
                        }
                      />
                    )}
                    <YStack flex={1} gap="$1">
                      <Text fontWeight="600">{`Step ${step.index + 1}`}</Text>
                      <Text>{step.text}</Text>
                    </YStack>
                  </XStack>
                  {editable &&
                    step.timers.map((timer) => (
                      <StepTimer
                        key={timer.index}
                        timer={timer}
                        disabled={disabled}
                        onAction={(action) =>
                          change(
                            new KitchenSessionChange.Timer({
                              step: step.index,
                              timer: timer.index,
                              action,
                            }),
                          )
                        }
                      />
                    ))}
                </YStack>
              </GymCard>
            ))}
            <H2 size="$5">Notes</H2>
            {view.notes.length === 0 && <Muted>No notes</Muted>}
            {view.notes.map((note, index) => (
              <Text key={index}>{note}</Text>
            ))}
            <H2 size="$5">Coming blocks</H2>
            {view.comingBlocks.length === 0 && <Muted>No cooking planned</Muted>}
            {view.comingBlocks.map((block) => (
              <GymCard key={`${block.itemId}:${block.occurrenceKey}`}>
                <YStack gap="$1">
                  <Text fontWeight="600">{block.title}</Text>
                  <Muted>{`${blockTime(block.startMillis)} – ${blockTime(block.endMillis)}`}</Muted>
                </YStack>
              </GymCard>
            ))}
            <H2 size="$5">Past sessions</H2>
            {view.pastSessions.length === 0 && <Muted>No finished sessions</Muted>}
            {view.pastSessions.map((session) => (
              <GymCard key={session.id}>
                <YStack gap="$1">
                  <Text>{localTime(session.startedAtMillis)}</Text>
                  <Muted>{`Took ${session.durationMinutes} min · Servings ${session.servings}`}</Muted>
                  {session.notes && <Text>{session.notes}</Text>}
                </YStack>
              </GymCard>
            ))}
          </YStack>
        </ScrollView>
      )}
      <SheetModal
        visible={finishing}
        title="How was it?"
        onClose={() => {
          if (!busy) setFinishing(false);
        }}
      >
        <NotesInput value={notes} onChange={setNotes} disabled={busy} />
        {error && <Text color={colors.bad}>{error}</Text>}
        <Button theme="green" disabled={disabled} onPress={() => void finish()}>
          Finish
        </Button>
        <Button disabled={busy} onPress={() => setFinishing(false)}>
          Cancel
        </Button>
      </SheetModal>
    </YStack>
  );
}

function Ingredient({ ingredient }: { ingredient: KitchenIngredient }) {
  return (
    <YStack flex={1} gap="$1">
      <Text>{`${ingredient.quantity} ${ingredient.name}`.trim()}</Text>
      {ingredient.optional && <Muted>Optional</Muted>}
      {ingredient.note && <Muted>{ingredient.note}</Muted>}
    </YStack>
  );
}
