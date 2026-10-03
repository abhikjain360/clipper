import { useCallback, useEffect, useRef, useState, type FormEvent } from "react";
import { Link, useLocation } from "wouter";
import { Button, H2, Input, Paragraph, Text, TextArea, XStack, YStack } from "tamagui";
import type { KitchenBackend, KitchenIngredient, KitchenSessionChange } from "@clipper/shared";
import { formatBackendError } from "../backend";
import { RecipeHistory } from "./RecipeHistory";
import { StepTimer } from "./StepTimer";
import {
    blockTime,
    Field,
    KitchenCard,
    KitchenDialog,
    kitchenZone,
    Loading,
    localTime,
    useKitchenData,
    type ErrorHandler,
} from "./shared";

let displayChange = Promise.resolve();

function keepDisplayAwake(backend: KitchenBackend, on: boolean, onError: ErrorHandler) {
    displayChange = displayChange
        .then(() => backend.keepDisplayAwake(on))
        .catch((error: unknown) => onError(formatBackendError(error)));
}

export function RecipePage({
    id,
    backend,
    state,
    onError,
}: {
    id: string;
    backend: KitchenBackend;
    state: unknown;
    onError: ErrorHandler;
}) {
    const [servings, setServings] = useState<number | null>(null);
    const [revision, setRevision] = useState<number | null>(null);
    const [version, setVersion] = useState(0);
    const [history, setHistory] = useState(false);
    const [action, setAction] = useState<"finish" | "discard" | null>(null);
    const [notes, setNotes] = useState("");
    const [busy, setBusy] = useState(false);
    const [changeError, setChangeError] = useState<string | null>(null);
    const changing = useRef(false);
    const mounted = useRef(false);
    const [, navigate] = useLocation();
    const load = useCallback(
        () =>
            revision === null
                ? backend.recipe(id, servings, kitchenZone())
                : backend.recipeRevision(id, revision, servings),
        [backend, id, servings, revision],
    );
    const { value: loaded, loading, failed, error } = useKitchenData(load, state, version, onError);
    const recipe =
        loaded &&
        (revision === null ? !loaded.read_only : loaded.read_only && loaded.revision === revision)
            ? loaded
            : null;
    useEffect(() => {
        mounted.current = true;
        keepDisplayAwake(backend, true, onError);
        return () => {
            mounted.current = false;
            keepDisplayAwake(backend, false, onError);
        };
    }, [backend, onError]);

    async function change(next: KitchenSessionChange) {
        if (changing.current || loading || failed || !recipe || recipe.read_only) return;
        changing.current = true;
        setBusy(true);
        setChangeError(null);
        onError(null);
        try {
            await backend.changeSession(id, recipe.revision, recipe.servings, next);
            if (recipe.deleted && (next.change === "finish" || next.change === "discard")) {
                navigate("/kitchen");
                return;
            }
            if (mounted.current) {
                setVersion((current) => current + 1);
                setAction(null);
                setNotes("");
            }
        } catch (cause) {
            if (mounted.current) {
                const message = formatBackendError(cause);
                setChangeError(message);
                onError(message);
            }
        } finally {
            changing.current = false;
            if (mounted.current) setBusy(false);
        }
    }

    const historyView = revision !== null || recipe?.read_only === true;
    const readOnly = historyView || recipe?.deleted === true;
    const disabled = busy || loading || failed;
    return (
        <YStack gap="$3" maxW={900} width="100%" self="center">
            <XStack gap="$2" items="center" flexWrap="wrap">
                <Link href="/kitchen" className="kitchen-recipe-link">
                    Back to Kitchen
                </Link>
                <Button
                    disabled={busy || loading}
                    onPress={() => setVersion((current) => current + 1)}
                >
                    Refresh
                </Button>
                <Button onPress={() => setHistory(true)}>History</Button>
                {revision !== null && (
                    <Button disabled={busy} onPress={() => setRevision(null)}>
                        Current recipe
                    </Button>
                )}
            </XStack>
            <Loading loading={loading} failed={failed} error={error} />
            {recipe && (
                <>
                    {recipe.read_only && <H2 size="$5">Revision {recipe.revision} (read-only)</H2>}
                    {recipe.deleted && (
                        <Paragraph role="status" color="#f3c969">
                            This recipe was deleted. Finish or discard this cooking session.
                        </Paragraph>
                    )}
                    {recipe.other_sessions.map((other) => (
                        <Paragraph key={other.id} role="status" color="#f3c969">
                            Another cooking session from revision {other.recipe_revision}, started{" "}
                            {localTime(other.started_at_millis)}, is open. It shows here once this
                            one is finished or discarded.
                        </Paragraph>
                    ))}
                    {recipe.newer_revision !== null && (
                        <Paragraph role="status" color="#f3c969">
                            This session cooks from revision {recipe.revision}. Revision{" "}
                            {recipe.newer_revision} is newer and applies from the next cook.
                        </Paragraph>
                    )}
                    <H2>{recipe.title}</H2>
                    <Paragraph>{recipe.summary}</Paragraph>
                    <XStack gap="$3" flexWrap="wrap">
                        <Text>{recipe.active_minutes} min active</Text>
                        <Text>{recipe.total_minutes} min total</Text>
                    </XStack>
                    {readOnly ? (
                        <Text>{recipe.servings} servings</Text>
                    ) : (
                        <Servings
                            key={recipe.servings}
                            servings={recipe.servings}
                            disabled={disabled}
                            onApply={(next) => {
                                if (recipe.session)
                                    void change({ change: "servings", servings: next });
                                else setServings(next);
                            }}
                        />
                    )}
                    {recipe.nutrition && (
                        <KitchenCard>
                            <H2 size="$5">Nutrition per serving</H2>
                            <XStack gap="$3" flexWrap="wrap">
                                <Text>{recipe.nutrition.calories} kcal</Text>
                                <Text>Protein: {recipe.nutrition.protein_grams} g</Text>
                                <Text>Carbs: {recipe.nutrition.carbs_grams} g</Text>
                                <Text>Fat: {recipe.nutrition.fat_grams} g</Text>
                                {recipe.nutrition.fiber_grams !== null && (
                                    <Text>Fiber: {recipe.nutrition.fiber_grams} g</Text>
                                )}
                            </XStack>
                        </KitchenCard>
                    )}
                    <KitchenCard>
                        <H2 size="$5">Ingredients</H2>
                        {recipe.groups.map((group, index) => (
                            <YStack key={index} gap="$2">
                                {group.name && <Text fontWeight="600">{group.name}</Text>}
                                {group.ingredients.map((ingredient) => (
                                    <YStack key={ingredient.id} gap="$1">
                                        {readOnly ? (
                                            <Ingredient ingredient={ingredient} />
                                        ) : (
                                            <label className="kitchen-check">
                                                <input
                                                    type="checkbox"
                                                    checked={ingredient.gathered}
                                                    disabled={disabled}
                                                    onChange={(event) =>
                                                        void change({
                                                            change: "gathered",
                                                            ingredient: ingredient.id,
                                                            gathered: event.target.checked,
                                                        })
                                                    }
                                                />
                                                <Ingredient ingredient={ingredient} />
                                            </label>
                                        )}
                                        {ingredient.note && (
                                            <Paragraph color="#9aa4ad" fontSize={13}>
                                                {ingredient.note}
                                            </Paragraph>
                                        )}
                                    </YStack>
                                ))}
                            </YStack>
                        ))}
                    </KitchenCard>
                    <KitchenCard>
                        <H2 size="$5">Shopping list</H2>
                        {recipe.shopping.length === 0 ? (
                            <Paragraph color="#9aa4ad">Nothing to buy.</Paragraph>
                        ) : (
                            recipe.shopping.map((ingredient) => (
                                <YStack key={ingredient.id} gap="$1">
                                    <Ingredient ingredient={ingredient} />
                                    {ingredient.note && (
                                        <Paragraph color="#9aa4ad">{ingredient.note}</Paragraph>
                                    )}
                                </YStack>
                            ))
                        )}
                    </KitchenCard>
                    <KitchenCard>
                        <H2 size="$5">Equipment</H2>
                        {recipe.equipment.length === 0 ? (
                            <Paragraph color="#9aa4ad">No equipment listed.</Paragraph>
                        ) : (
                            recipe.equipment.map((name, index) => <Text key={index}>{name}</Text>)
                        )}
                    </KitchenCard>
                    <KitchenCard>
                        <H2 size="$5">Steps</H2>
                        {recipe.steps.map((step) => (
                            <YStack key={step.index} gap="$2">
                                {readOnly ? (
                                    <Paragraph>
                                        {step.index + 1}. {step.text}
                                    </Paragraph>
                                ) : (
                                    <label className="kitchen-check">
                                        <input
                                            type="checkbox"
                                            checked={step.done}
                                            disabled={disabled}
                                            onChange={(event) =>
                                                void change({
                                                    change: "step_done",
                                                    step: step.index,
                                                    done: event.target.checked,
                                                })
                                            }
                                        />
                                        <Paragraph>
                                            {step.index + 1}. {step.text}
                                        </Paragraph>
                                    </label>
                                )}
                                {!readOnly &&
                                    step.timers.map((timer) => (
                                        <StepTimer
                                            key={timer.index}
                                            timer={timer}
                                            disabled={disabled}
                                            onAction={(timerAction) =>
                                                void change({
                                                    change: "timer",
                                                    step: step.index,
                                                    timer: timer.index,
                                                    action: timerAction,
                                                })
                                            }
                                        />
                                    ))}
                            </YStack>
                        ))}
                    </KitchenCard>
                    <KitchenCard>
                        <H2 size="$5">Notes</H2>
                        {recipe.notes.length === 0 ? (
                            <Paragraph color="#9aa4ad">No notes.</Paragraph>
                        ) : (
                            recipe.notes.map((note, index) => (
                                <Paragraph key={index}>{note}</Paragraph>
                            ))
                        )}
                    </KitchenCard>
                    <KitchenCard>
                        <H2 size="$5">Coming blocks</H2>
                        {recipe.coming_blocks.length === 0 ? (
                            <Paragraph color="#9aa4ad">No coming blocks.</Paragraph>
                        ) : (
                            recipe.coming_blocks.map((block) => (
                                <Paragraph key={`${block.item_id}:${block.occurrence_key}`}>
                                    {block.title}, {blockTime(block.start_millis)}
                                </Paragraph>
                            ))
                        )}
                    </KitchenCard>
                    <KitchenCard>
                        <H2 size="$5">Past sessions</H2>
                        {recipe.past_sessions.length === 0 && (
                            <Paragraph color="#9aa4ad">No past sessions.</Paragraph>
                        )}
                        {recipe.past_sessions.map((session) => (
                            <YStack key={session.id} gap="$1">
                                <Text>
                                    {session.servings} servings · Revision {session.recipe_revision}
                                </Text>
                                <Text color="#9aa4ad">
                                    Started: {localTime(session.started_at_millis)}
                                </Text>
                                <Text color="#9aa4ad">
                                    Finished: {localTime(session.finished_at_millis)} · took{" "}
                                    {session.duration_minutes} min
                                </Text>
                                {session.notes && <Paragraph>{session.notes}</Paragraph>}
                            </YStack>
                        ))}
                    </KitchenCard>
                    {!historyView && (
                        <XStack gap="$2" flexWrap="wrap">
                            {recipe.session ? (
                                <>
                                    <Button
                                        theme="blue"
                                        disabled={disabled}
                                        onPress={() => setAction("finish")}
                                    >
                                        Finish
                                    </Button>
                                    <Button
                                        theme="red"
                                        disabled={disabled}
                                        onPress={() => setAction("discard")}
                                    >
                                        Discard
                                    </Button>
                                </>
                            ) : (
                                <Button
                                    theme="blue"
                                    disabled={disabled}
                                    onPress={() => void change({ change: "start" })}
                                >
                                    Start cooking
                                </Button>
                            )}
                        </XStack>
                    )}
                </>
            )}
            <KitchenDialog
                open={history}
                title="History"
                description="Recipe revisions, oldest first."
                onClose={() => setHistory(false)}
            >
                {history && (
                    <RecipeHistory
                        id={id}
                        backend={backend}
                        state={state}
                        onError={onError}
                        onRevision={(next) => {
                            setRevision(next);
                            setHistory(false);
                        }}
                    />
                )}
            </KitchenDialog>
            <KitchenDialog
                open={action !== null}
                title={action === "finish" ? "How was it?" : "Discard this cooking session?"}
                description={
                    action === "finish"
                        ? "Save notes about this cook."
                        : "This removes the session and clears its timers."
                }
                busy={busy}
                onClose={() => {
                    setAction(null);
                    setNotes("");
                }}
            >
                {changeError && (
                    <Paragraph role="alert" color="#ff7b7b">
                        {changeError}
                    </Paragraph>
                )}
                {action === "finish" && (
                    <Field label="Notes">
                        <TextArea
                            value={notes}
                            onChangeText={setNotes}
                            disabled={busy}
                            minH={120}
                        />
                    </Field>
                )}
                <Button
                    disabled={disabled}
                    theme={action === "discard" ? "red" : "blue"}
                    onPress={() => {
                        if (action === "finish")
                            void change({ change: "finish", notes: notes.trim() || null });
                        else if (action === "discard") void change({ change: "discard" });
                    }}
                >
                    {action === "finish" ? "Finish cooking" : "Discard session"}
                </Button>
            </KitchenDialog>
        </YStack>
    );
}

function Ingredient({ ingredient }: { ingredient: KitchenIngredient }) {
    return (
        <Text>
            {ingredient.quantity ? `${ingredient.quantity} ` : ""}
            {ingredient.name}
            {ingredient.optional ? " · Optional" : ""}
        </Text>
    );
}

function Servings({
    servings,
    disabled,
    onApply,
}: {
    servings: number;
    disabled: boolean;
    onApply: (servings: number) => void;
}) {
    const [value, setValue] = useState(String(servings));
    function submit(event: FormEvent<HTMLFormElement>) {
        event.preventDefault();
        onApply(Number(value));
    }
    return (
        <form onSubmit={submit}>
            <XStack gap="$2" items="flex-end" flexWrap="wrap">
                <Field label="Servings">
                    <Input
                        type="number"
                        min={1}
                        step={1}
                        required
                        value={value}
                        onChangeText={setValue}
                        disabled={disabled}
                        width={100}
                    />
                </Field>
                <Button type="submit" disabled={disabled}>
                    Apply
                </Button>
            </XStack>
        </form>
    );
}
