import { Button } from "../tamagui.config";
import { palette, statusSurfaces } from "@clipper/shared";
import { useCallback, useState } from "react";
import { Link } from "wouter";
import { H2, Input, Paragraph, Text, XStack, YStack } from "tamagui";
import type { KitchenBackend, KitchenRecipeSummary } from "@clipper/shared";
import {
    blockTime,
    Field,
    KitchenCard,
    kitchenZone,
    Loading,
    localTime,
    useKitchenData,
    type ErrorHandler,
} from "./shared";

export function RecipeList({
    backend,
    state,
    onError,
}: {
    backend: KitchenBackend;
    state: unknown;
    onError: ErrorHandler;
}) {
    const [search, setSearch] = useState("");
    const [version, setVersion] = useState(0);
    const load = useCallback(() => backend.recipes(search, kitchenZone()), [backend, search]);
    const { value, loading, failed } = useKitchenData(load, state, version, onError);
    return (
        <YStack gap="$3">
            <XStack gap="$2" items="flex-end" flexWrap="wrap">
                <YStack flex={1} minW={180}>
                    <Field label="Search recipes">
                        <Input
                            value={search}
                            onChangeText={setSearch}
                            placeholder="Title, summary, cuisine or tags"
                        />
                    </Field>
                </YStack>
                <Button onPress={() => setVersion((current) => current + 1)} disabled={loading}>
                    Refresh
                </Button>
            </XStack>
            <Loading loading={loading} failed={failed} />
            {value && value.open_sessions.length > 0 && (
                <KitchenCard>
                    <H2 size="$5">Cooking now</H2>
                    {value.open_sessions.map((session) => (
                        <YStack key={session.session_id} gap="$1">
                            <Link
                                href={`/kitchen/${session.recipe_id}`}
                                className="kitchen-recipe-link"
                            >
                                {session.title}
                            </Link>
                            <Text color={palette.secondary}>
                                Started {localTime(session.started_at_millis)} · Revision{" "}
                                {session.recipe_revision}
                                {session.deleted ? " · Recipe deleted" : ""}
                            </Text>
                        </YStack>
                    ))}
                </KitchenCard>
            )}
            {!loading && !failed && !search.trim() && value?.next_block && (
                <KitchenCard>
                    <H2 size="$5">
                        Next cooking: {value.next_block.title},{" "}
                        {blockTime(value.next_block.start_millis)}
                    </H2>
                    {value.next_block_recipes.map((recipe) => (
                        <RecipeSummary key={recipe.id} recipe={recipe} />
                    ))}
                </KitchenCard>
            )}
            <H2 size="$5">Recipes</H2>
            {value?.recipes.map((recipe) => (
                <KitchenCard key={recipe.id}>
                    <RecipeSummary recipe={recipe} />
                </KitchenCard>
            ))}
            {!loading && !failed && value?.recipes.length === 0 && (
                <Paragraph color={palette.secondary}>
                    {search.trim() ? "No recipes match this search." : "No recipes yet."}
                </Paragraph>
            )}
        </YStack>
    );
}

function RecipeSummary({ recipe }: { recipe: KitchenRecipeSummary }) {
    return (
        <YStack gap="$2">
            <XStack gap="$2" items="center" flexWrap="wrap">
                <Link href={`/kitchen/${recipe.id}`} className="kitchen-recipe-link">
                    {recipe.title}
                </Link>
                {recipe.cooking && (
                    <Text
                        bg={statusSurfaces.success}
                        color={palette.success}
                        px="$2"
                        py="$1"
                        rounded="$2"
                        fontSize={12}
                    >
                        Cooking
                    </Text>
                )}
            </XStack>
            <Paragraph>{recipe.summary}</Paragraph>
            <XStack gap="$2" flexWrap="wrap">
                {recipe.cuisine && <Text color={palette.secondary}>{recipe.cuisine}</Text>}
                {recipe.tags.map((tag) => (
                    <Text key={tag} color={palette.secondary}>
                        {tag}
                    </Text>
                ))}
                <Text color={palette.secondary}>{recipe.total_minutes} min total</Text>
            </XStack>
            {recipe.next_block && (
                <Paragraph color={palette.secondary}>
                    Planned: {recipe.next_block.title}, {blockTime(recipe.next_block.start_millis)}
                </Paragraph>
            )}
        </YStack>
    );
}
