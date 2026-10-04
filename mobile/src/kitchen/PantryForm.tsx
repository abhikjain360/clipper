import {
  KitchenPantryChange,
  type KitchenEquipment,
  type KitchenPantryItem,
} from "@clipper/mobile-bridge";
import { useState } from "react";
import { Button, Input, Text, XStack } from "tamagui";
import { colors, SheetModal } from "../gym/GymUi";
import { Field, NotesInput } from "./KitchenUi";

export type PantryDraft =
  | { kind: "item"; item?: KitchenPantryItem }
  | { kind: "equipment"; item?: KitchenEquipment };

export function PantryForm({
  draft,
  categories,
  busy,
  error,
  onClose,
  onSave,
}: {
  draft: PantryDraft;
  categories: string[];
  busy: boolean;
  error: string | null;
  onClose: () => void;
  onSave: (change: KitchenPantryChange) => void;
}) {
  const [name, setName] = useState(draft.item?.name ?? "");
  const [category, setCategory] = useState(
    draft.kind === "item" ? (draft.item?.category ?? "") : "",
  );
  const [amount, setAmount] = useState(draft.kind === "item" ? (draft.item?.amount ?? "") : "");
  const [useBy, setUseBy] = useState(draft.kind === "item" ? (draft.item?.useBy ?? "") : "");
  const [notes, setNotes] = useState(draft.item?.notes ?? "");

  function save() {
    if (busy) return;
    onSave(
      draft.kind === "item"
        ? new KitchenPantryChange.SaveItem({
            id: draft.item?.id,
            name,
            category,
            amount: amount.trim() || undefined,
            useBy: useBy.trim() || undefined,
            notes: notes.trim() || undefined,
          })
        : new KitchenPantryChange.SaveEquipment({
            id: draft.item?.id,
            name,
            notes: notes.trim() || undefined,
          }),
    );
  }

  return (
    <SheetModal
      visible
      title={`${draft.item ? "Edit" : "Add"} ${draft.kind === "item" ? "pantry item" : "equipment"}`}
      onClose={() => {
        if (!busy) onClose();
      }}
    >
      <Field label="Name">
        <Input aria-label="Name" value={name} onChangeText={setName} disabled={busy} />
      </Field>
      {draft.kind === "item" && (
        <>
          <Field label="Category">
            <Input
              aria-label="Category"
              value={category}
              onChangeText={setCategory}
              disabled={busy}
            />
            <XStack flexWrap="wrap" gap="$2">
              {categories.map((value) => (
                <Button
                  key={value}
                  size="$3"
                  theme={value === category ? "blue" : undefined}
                  disabled={busy}
                  onPress={() => setCategory(value)}
                >
                  {value}
                </Button>
              ))}
            </XStack>
          </Field>
          <Field label="Amount">
            <Input
              aria-label="Amount"
              value={amount}
              onChangeText={setAmount}
              disabled={busy}
              placeholder="250 g"
            />
          </Field>
          <Field label="Use-by (YYYY-MM-DD)">
            <Input
              aria-label="Use-by date"
              value={useBy}
              onChangeText={setUseBy}
              disabled={busy}
              placeholder="YYYY-MM-DD"
              autoCapitalize="none"
              autoCorrect={false}
            />
          </Field>
        </>
      )}
      <Field label="Notes">
        <NotesInput value={notes} onChange={setNotes} disabled={busy} />
      </Field>
      {error && <Text color={colors.bad}>{error}</Text>}
      <Button theme="blue" disabled={busy} onPress={save}>
        Save
      </Button>
      <Button disabled={busy} onPress={onClose}>
        Cancel
      </Button>
    </SheetModal>
  );
}
