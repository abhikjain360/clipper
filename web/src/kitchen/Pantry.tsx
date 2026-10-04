import { Button, Input, TextArea } from "../tamagui.config";
import { palette } from "@clipper/shared";
import { useCallback, useRef, useState, type FormEvent } from "react";
import { H2, Paragraph, Text, XStack, YStack } from "tamagui";
import type {
    KitchenBackend,
    KitchenEquipment,
    KitchenPantryChange,
    KitchenPantryItem,
} from "@clipper/shared";
import { formatBackendError } from "../backend";
import {
    Field,
    KitchenCard,
    KitchenDialog,
    Loading,
    useKitchenData,
    type ErrorHandler,
} from "./shared";

type Editor =
    | { kind: "item"; item: KitchenPantryItem | null }
    | { kind: "equipment"; item: KitchenEquipment | null };

export function Pantry({
    backend,
    state,
    onError,
}: {
    backend: KitchenBackend;
    state: unknown;
    onError: ErrorHandler;
}) {
    const [version, setVersion] = useState(0);
    const [editor, setEditor] = useState<Editor | null>(null);
    const [deleting, setDeleting] = useState<{
        kind: "item" | "equipment";
        id: string;
        name: string;
    } | null>(null);
    const [busy, setBusy] = useState(false);
    const [changeError, setChangeError] = useState<string | null>(null);
    const changing = useRef(false);
    const load = useCallback(() => backend.pantry(), [backend]);
    const { value, loading, failed } = useKitchenData(load, state, version, onError);

    async function change(next: KitchenPantryChange) {
        if (changing.current) return;
        changing.current = true;
        setBusy(true);
        setChangeError(null);
        onError(null);
        try {
            await backend.changePantry(next);
            setEditor(null);
            setDeleting(null);
            setVersion((current) => current + 1);
        } catch (error) {
            const message = formatBackendError(error);
            setChangeError(message);
            onError(message);
        } finally {
            changing.current = false;
            setBusy(false);
        }
    }

    return (
        <YStack gap="$3">
            <XStack gap="$2" flexWrap="wrap">
                <Button
                    disabled={busy || loading}
                    onPress={() => setVersion((current) => current + 1)}
                >
                    Refresh
                </Button>
                <Button
                    tone="accent"
                    disabled={busy || loading || failed}
                    onPress={() => setEditor({ kind: "item", item: null })}
                >
                    Add item
                </Button>
            </XStack>
            <Loading loading={loading} failed={failed} />
            {value?.categories.map((category) => (
                <KitchenCard key={category.name}>
                    <H2 size="$5">{category.name}</H2>
                    {category.items.map((item) => (
                        <YStack key={item.id} gap="$1">
                            <XStack gap="$2" items="center" flexWrap="wrap">
                                <Text fontWeight="600">{item.name}</Text>
                                {item.listed_twice && (
                                    <Text color={palette.warning}>Listed twice</Text>
                                )}
                            </XStack>
                            {item.amount && <Text>{item.amount}</Text>}
                            {item.use_by && (
                                <Text color={palette.secondary}>Use by: {item.use_by}</Text>
                            )}
                            {item.notes && <Paragraph>{item.notes}</Paragraph>}
                            <XStack gap="$2">
                                <Button
                                    size="$2"
                                    disabled={busy || loading || failed}
                                    aria-label={`Edit ${item.name}`}
                                    onPress={() => setEditor({ kind: "item", item })}
                                >
                                    Edit
                                </Button>
                                <Button
                                    size="$2"
                                    disabled={busy || loading || failed}
                                    aria-label={`Delete ${item.name}`}
                                    onPress={() =>
                                        setDeleting({ kind: "item", id: item.id, name: item.name })
                                    }
                                >
                                    Delete
                                </Button>
                            </XStack>
                        </YStack>
                    ))}
                </KitchenCard>
            ))}
            {!loading && !failed && value?.categories.length === 0 && (
                <Paragraph color={palette.secondary}>No pantry items yet.</Paragraph>
            )}
            <XStack items="center" justify="space-between" gap="$2" flexWrap="wrap">
                <H2 size="$5">Equipment</H2>
                <Button
                    disabled={busy || loading || failed}
                    onPress={() => setEditor({ kind: "equipment", item: null })}
                >
                    Add equipment
                </Button>
            </XStack>
            {value?.equipment.map((item) => (
                <KitchenCard key={item.id}>
                    <XStack gap="$2" items="center" flexWrap="wrap">
                        <Text fontWeight="600">{item.name}</Text>
                        {item.listed_twice && <Text color={palette.warning}>Listed twice</Text>}
                    </XStack>
                    {item.notes && <Paragraph>{item.notes}</Paragraph>}
                    <XStack gap="$2">
                        <Button
                            size="$2"
                            disabled={busy || loading || failed}
                            aria-label={`Edit ${item.name}`}
                            onPress={() => setEditor({ kind: "equipment", item })}
                        >
                            Edit
                        </Button>
                        <Button
                            size="$2"
                            disabled={busy || loading || failed}
                            aria-label={`Delete ${item.name}`}
                            onPress={() =>
                                setDeleting({ kind: "equipment", id: item.id, name: item.name })
                            }
                        >
                            Delete
                        </Button>
                    </XStack>
                </KitchenCard>
            ))}
            {!loading && !failed && value?.equipment.length === 0 && (
                <Paragraph color={palette.secondary}>No equipment yet.</Paragraph>
            )}
            <KitchenDialog
                open={editor !== null}
                title={`${editor?.item ? "Edit" : "Add"} ${editor?.kind === "equipment" ? "equipment" : "pantry item"}`}
                description="Save the name and details."
                busy={busy}
                onClose={() => {
                    setEditor(null);
                    setChangeError(null);
                }}
            >
                {changeError && (
                    <Paragraph role="alert" color={palette.danger}>
                        {changeError}
                    </Paragraph>
                )}
                {editor && (
                    <PantryForm
                        editor={editor}
                        categories={value?.categories.map((category) => category.name) ?? []}
                        busy={busy}
                        onSave={(next) => void change(next)}
                    />
                )}
            </KitchenDialog>
            <KitchenDialog
                open={deleting !== null}
                title={`Delete ${deleting?.name ?? "item"}?`}
                description="This removes the item from the list."
                busy={busy}
                onClose={() => {
                    setDeleting(null);
                    setChangeError(null);
                }}
            >
                {changeError && (
                    <Paragraph role="alert" color={palette.danger}>
                        {changeError}
                    </Paragraph>
                )}
                <Button
                    tone="danger"
                    disabled={busy}
                    onPress={() => {
                        if (deleting)
                            void change({
                                change:
                                    deleting.kind === "item" ? "delete_item" : "delete_equipment",
                                id: deleting.id,
                            });
                    }}
                >
                    Delete
                </Button>
            </KitchenDialog>
        </YStack>
    );
}

function PantryForm({
    editor,
    categories,
    busy,
    onSave,
}: {
    editor: Editor;
    categories: string[];
    busy: boolean;
    onSave: (change: KitchenPantryChange) => void;
}) {
    const pantryItem = editor.kind === "item" ? editor.item : null;
    const [name, setName] = useState(editor.item?.name ?? "");
    const [category, setCategory] = useState(pantryItem?.category ?? "");
    const [amount, setAmount] = useState(pantryItem?.amount ?? "");
    const [useBy, setUseBy] = useState(pantryItem?.use_by ?? "");
    const [notes, setNotes] = useState(editor.item?.notes ?? "");
    function submit(event: FormEvent<HTMLFormElement>) {
        event.preventDefault();
        if (editor.kind === "equipment")
            onSave({
                change: "save_equipment",
                id: editor.item?.id ?? null,
                name,
                notes: notes.trim() || null,
            });
        else
            onSave({
                change: "save_item",
                id: editor.item?.id ?? null,
                name,
                category,
                amount: amount.trim() || null,
                use_by: useBy || null,
                notes: notes.trim() || null,
            });
    }
    return (
        <form onSubmit={submit}>
            <YStack gap="$3">
                <Field label="Name">
                    <Input value={name} onChangeText={setName} disabled={busy} autoFocus />
                </Field>
                {editor.kind === "item" && (
                    <>
                        <Field label="Category">
                            <Input
                                value={category}
                                onChangeText={setCategory}
                                disabled={busy}
                                list="kitchen-categories"
                            />
                        </Field>
                        <datalist id="kitchen-categories">
                            {categories.map((categoryName) => (
                                <option key={categoryName} value={categoryName} />
                            ))}
                        </datalist>
                        <Field label="Amount">
                            <Input
                                value={amount}
                                onChangeText={setAmount}
                                disabled={busy}
                                placeholder="250 g"
                            />
                        </Field>
                        <Field label="Use by">
                            <Input
                                type="date"
                                value={useBy}
                                onChangeText={setUseBy}
                                disabled={busy}
                            />
                        </Field>
                    </>
                )}
                <Field label="Notes">
                    <TextArea value={notes} onChangeText={setNotes} disabled={busy} minH={100} />
                </Field>
                <Button tone="accent" type="submit" disabled={busy}>
                    Save
                </Button>
            </YStack>
        </form>
    );
}
