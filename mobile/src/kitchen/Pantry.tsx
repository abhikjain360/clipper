import { Button } from "../tamagui.config";
import { KitchenPantryChange } from "@clipper/mobile-bridge";
import type { AppState } from "@clipper/shared";
import { Pencil, Plus, Trash2 } from "lucide-react-native";
import { useCallback, useState } from "react";
import { Alert } from "react-native";
import { H2, ScrollView, Text, XStack, YStack } from "tamagui";
import { colors, GymCard, Muted } from "../gym/GymUi";
import { kitchen, useKitchenChange, useKitchenView } from "./kitchenClient";
import { LoadStatus } from "./KitchenUi";
import { PantryForm, type PantryDraft } from "./PantryForm";

export function Pantry({
  state,
  onError,
}: {
  state: AppState;
  onError: (error: string | null) => void;
}) {
  const query = useCallback(() => kitchen().kitchenPantry(), []);
  const { view, loading, failed, reload } = useKitchenView(query, state, onError);
  const { busy, error, run } = useKitchenChange(reload, onError);
  const [draft, setDraft] = useState<PantryDraft | null>(null);
  const disabled = busy || loading || failed;

  function remove(name: string, change: KitchenPantryChange) {
    Alert.alert(`Delete ${name}?`, "It is removed from the kitchen on every device.", [
      { text: "Cancel", style: "cancel" },
      {
        text: "Delete",
        style: "destructive",
        onPress: () => void run(() => kitchen().kitchenChangePantry(change)),
      },
    ]);
  }

  async function save(change: KitchenPantryChange) {
    if (await run(() => kitchen().kitchenChangePantry(change))) setDraft(null);
  }

  return (
    <YStack flex={1} gap="$3">
      <LoadStatus loading={loading} failed={failed} onRetry={reload} />
      <ScrollView flex={1} keyboardShouldPersistTaps="always">
        <YStack gap="$3" pb="$8">
          <XStack items="center" justify="space-between" gap="$2">
            <H2 size="$6">Pantry</H2>
            <Button
              size="$3"
              icon={<Plus size={16} />}
              disabled={disabled}
              onPress={() => setDraft({ kind: "item" })}
            >
              Add item
            </Button>
          </XStack>
          {view?.categories.length === 0 && !loading && <Muted>No pantry items</Muted>}
          {view?.categories.map((category) => (
            <YStack key={category.name} gap="$2">
              <Text fontWeight="600">{category.name}</Text>
              {category.items.map((item) => (
                <GymCard key={item.id}>
                  <YStack gap="$2">
                    <Text fontWeight="600">{item.name}</Text>
                    {item.amount && <Muted>{item.amount}</Muted>}
                    {item.useBy && <Muted>{`Use-by ${item.useBy}`}</Muted>}
                    {item.notes && <Text>{item.notes}</Text>}
                    {item.listedTwice && <Text color={colors.warm}>Listed twice</Text>}
                    <XStack gap="$2">
                      <Button
                        size="$3"
                        icon={<Pencil size={16} />}
                        disabled={disabled}
                        aria-label={`Edit ${item.name}`}
                        onPress={() => setDraft({ kind: "item", item })}
                      >
                        Edit
                      </Button>
                      <Button
                        size="$3"
                        icon={<Trash2 size={16} color={colors.bad} />}
                        disabled={disabled}
                        aria-label={`Delete ${item.name}`}
                        onPress={() =>
                          remove(item.name, new KitchenPantryChange.DeleteItem({ id: item.id }))
                        }
                      >
                        Delete
                      </Button>
                    </XStack>
                  </YStack>
                </GymCard>
              ))}
            </YStack>
          ))}
          <XStack items="center" justify="space-between" gap="$2">
            <H2 size="$6">Equipment</H2>
            <Button
              size="$3"
              icon={<Plus size={16} />}
              disabled={disabled}
              onPress={() => setDraft({ kind: "equipment" })}
            >
              Add equipment
            </Button>
          </XStack>
          {view?.equipment.length === 0 && !loading && <Muted>No equipment</Muted>}
          {view?.equipment.map((item) => (
            <GymCard key={item.id}>
              <YStack gap="$2">
                <Text fontWeight="600">{item.name}</Text>
                {item.notes && <Text>{item.notes}</Text>}
                {item.listedTwice && <Text color={colors.warm}>Listed twice</Text>}
                <XStack gap="$2">
                  <Button
                    size="$3"
                    icon={<Pencil size={16} />}
                    disabled={disabled}
                    aria-label={`Edit ${item.name}`}
                    onPress={() => setDraft({ kind: "equipment", item })}
                  >
                    Edit
                  </Button>
                  <Button
                    size="$3"
                    icon={<Trash2 size={16} color={colors.bad} />}
                    disabled={disabled}
                    aria-label={`Delete ${item.name}`}
                    onPress={() =>
                      remove(item.name, new KitchenPantryChange.DeleteEquipment({ id: item.id }))
                    }
                  >
                    Delete
                  </Button>
                </XStack>
              </YStack>
            </GymCard>
          ))}
        </YStack>
      </ScrollView>
      {draft && (
        <PantryForm
          draft={draft}
          categories={view?.categories.map((category) => category.name) ?? []}
          busy={busy}
          error={error}
          onClose={() => setDraft(null)}
          onSave={(change) => void save(change)}
        />
      )}
    </YStack>
  );
}
