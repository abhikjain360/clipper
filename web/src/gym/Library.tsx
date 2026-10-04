import { ChevronDown, ChevronUp, Plus, Trash2 } from "lucide-react";
import { useCallback, useState, type ReactNode } from "react";
import { Label, ScrollView, Spinner, Switch, Text, XStack, YStack } from "tamagui";
import { Input } from "../tamagui.config";
import { Button } from "./Button";
import {
    formatRestSeconds,
    palette,
    type GymBackend,
    type GymExercise,
    type GymMuscleInfo,
    type GymMuscleShare,
    type GymPlannedExercise,
} from "@clipper/shared";
import { formatBackendError } from "../backend";
import {
    CardTitle,
    Column,
    Columns,
    ConfirmDialog,
    ExercisePicker,
    GymCard,
    GymDialog,
    Muted,
    muscleGroups,
    Stepper,
    useGymChange,
    useGymData,
    type ErrorHandler,
} from "./GymUi";

type ExerciseDraft = {
    id: string | null;
    name: string;
    muscles: GymMuscleShare[];
    archived: boolean;
};
type TemplateDraft = { id: string | null; name: string; exercises: GymPlannedExercise[] };

export function Library({
    backend,
    state,
    onError,
}: {
    backend: GymBackend;
    state: unknown;
    onError: ErrorHandler;
}) {
    const load = useCallback(
        () => Promise.all([backend.exercises(), backend.templates(), backend.muscles()]),
        [backend],
    );
    const { value, reload } = useGymData(load, state, onError);
    const { busy, run } = useGymChange(reload, onError);
    const [search, setSearch] = useState("");
    const [exerciseDraft, setExerciseDraft] = useState<ExerciseDraft | null>(null);
    const [templateDraft, setTemplateDraft] = useState<TemplateDraft | null>(null);
    const [deletingTemplate, setDeletingTemplate] = useState<TemplateDraft | null>(null);

    if (value === undefined) return <Spinner />;
    const [exercises, templates, muscles] = value;
    const names = new Map(exercises.map((exercise) => [exercise.id, exercise.name]));
    const displayNames = new Map(muscles.map((muscle) => [muscle.muscle, muscle.display_name]));
    const query = search.trim().toLowerCase();
    const shown = [
        ...exercises.filter((exercise) => !exercise.archived),
        ...exercises.filter((exercise) => exercise.archived),
    ].filter((exercise) => exercise.name.toLowerCase().includes(query));

    return (
        <Columns>
            <Column minWidth={340}>
                <GymCard>
                    <XStack items="center" justify="space-between" gap="$2">
                        <CardTitle>Workouts</CardTitle>
                        <Button
                            size="$3"
                            icon={<Plus size={14} />}
                            onPress={() => setTemplateDraft({ id: null, name: "", exercises: [] })}
                        >
                            New workout
                        </Button>
                    </XStack>
                    {templates.length === 0 && <Muted>No workouts yet</Muted>}
                    <YStack gap="$1">
                        {templates.map((template) => (
                            <ListRow
                                key={template.id}
                                label={`Edit ${template.name}`}
                                selected={template.id === templateDraft?.id}
                                onPress={() =>
                                    setTemplateDraft({
                                        id: template.id,
                                        name: template.name,
                                        exercises: template.exercises.map((planned) => ({
                                            ...planned,
                                        })),
                                    })
                                }
                            >
                                <Text fontWeight="600">{template.name}</Text>
                                <Muted>
                                    {template.exercises
                                        .map(
                                            (planned) =>
                                                names.get(planned.exercise_id) ??
                                                "Unknown exercise",
                                        )
                                        .join(", ")}
                                </Muted>
                            </ListRow>
                        ))}
                    </YStack>
                </GymCard>
            </Column>
            <Column minWidth={340}>
                <GymCard>
                    <XStack items="center" justify="space-between" gap="$2">
                        <CardTitle>Exercises</CardTitle>
                        <Button
                            size="$3"
                            icon={<Plus size={14} />}
                            onPress={() =>
                                setExerciseDraft({
                                    id: null,
                                    name: "",
                                    muscles: [],
                                    archived: false,
                                })
                            }
                        >
                            New exercise
                        </Button>
                    </XStack>
                    <Input
                        value={search}
                        onChangeText={setSearch}
                        placeholder="Search exercises"
                        aria-label="Search exercises"
                    />
                    <ScrollView maxH="65vh">
                        <YStack gap="$1">
                            {shown.length === 0 && <Muted>No matching exercises</Muted>}
                            {shown.map((exercise) => (
                                <ListRow
                                    key={exercise.id}
                                    label={`Edit ${exercise.name}`}
                                    selected={exercise.id === exerciseDraft?.id}
                                    onPress={() =>
                                        setExerciseDraft({
                                            id: exercise.id,
                                            name: exercise.name,
                                            muscles: exercise.muscles.map((share) => ({
                                                ...share,
                                            })),
                                            archived: exercise.archived,
                                        })
                                    }
                                >
                                    <XStack justify="space-between" gap="$2">
                                        <Text
                                            color={
                                                exercise.archived ? palette.secondary : undefined
                                            }
                                        >
                                            {exercise.name}
                                        </Text>
                                        {exercise.archived && <Muted>archived</Muted>}
                                    </XStack>
                                    <Muted>{mainMuscles(exercise, displayNames)}</Muted>
                                </ListRow>
                            ))}
                        </YStack>
                    </ScrollView>
                </GymCard>
            </Column>
            <ExerciseEditor
                draft={exerciseDraft}
                muscles={muscles}
                busy={busy}
                onChange={setExerciseDraft}
                onClose={() => setExerciseDraft(null)}
                onSave={(draft) =>
                    void run(() =>
                        backend.change({
                            change: "save_exercise",
                            id: draft.id,
                            exercise: {
                                name: draft.name,
                                muscles: draft.muscles,
                                archived: draft.archived,
                            },
                        }),
                    ).then((saved) => saved && setExerciseDraft(null))
                }
            />
            <TemplateEditor
                draft={templateDraft}
                exercises={exercises}
                backend={backend}
                busy={busy}
                onError={onError}
                onChange={setTemplateDraft}
                onClose={() => setTemplateDraft(null)}
                onSave={(draft) =>
                    void run(() =>
                        backend.change({
                            change: "save_template",
                            id: draft.id,
                            name: draft.name,
                            exercises: draft.exercises,
                        }),
                    ).then((saved) => saved && setTemplateDraft(null))
                }
                onDelete={(draft) => setDeletingTemplate(draft)}
            />
            <ConfirmDialog
                open={deletingTemplate !== null}
                title={`Delete ${deletingTemplate?.name ?? "this workout"}?`}
                description="Past workouts keep their sets."
                confirmLabel="Delete workout"
                busy={busy}
                onCancel={() => setDeletingTemplate(null)}
                onConfirm={() => {
                    const id = deletingTemplate?.id;
                    if (id)
                        void run(() => backend.change({ change: "delete_template", id })).then(
                            (deleted) => {
                                if (deleted) {
                                    setDeletingTemplate(null);
                                    setTemplateDraft(null);
                                }
                            },
                        );
                }}
            />
        </Columns>
    );
}

function ListRow({
    label,
    selected,
    onPress,
    children,
}: {
    label: string;
    selected: boolean;
    onPress: () => void;
    children: ReactNode;
}) {
    return (
        <YStack
            gap="$1"
            px="$3"
            py="$2"
            rounded="$3"
            bg={selected ? palette.selectedFill : palette.cardFill}
            borderWidth={selected ? 1 : 0}
            borderColor={palette.selectedBorder}
            cursor="pointer"
            hoverStyle={{ bg: selected ? palette.selectedFill : palette.cardFill }}
            pressStyle={{ bg: selected ? palette.selectedFill : palette.cardFill }}
            role="button"
            aria-pressed={selected}
            aria-label={label}
            onPress={onPress}
        >
            {children}
        </YStack>
    );
}

function mainMuscles(exercise: GymExercise, displayNames: Map<string, string>): string {
    return exercise.muscles
        .toSorted((a, b) => b.share - a.share)
        .map((share) => displayNames.get(share.muscle) ?? share.muscle)
        .join(", ");
}

function ExerciseEditor({
    draft,
    muscles,
    busy,
    onChange,
    onClose,
    onSave,
}: {
    draft: ExerciseDraft | null;
    muscles: GymMuscleInfo[];
    busy: boolean;
    onChange: (draft: ExerciseDraft) => void;
    onClose: () => void;
    onSave: (draft: ExerciseDraft) => void;
}) {
    function setShare(muscle: GymMuscleInfo, share: number) {
        if (!draft) return;
        const others = draft.muscles.filter((target) => target.muscle !== muscle.muscle);
        onChange({
            ...draft,
            muscles: share > 0 ? [...others, { muscle: muscle.muscle, share }] : others,
        });
    }
    return (
        <GymDialog
            open={draft !== null}
            title={draft?.id ? "Edit exercise" : "New exercise"}
            onClose={onClose}
            busy={busy}
            width={760}
        >
            {draft && (
                <>
                    <YStack gap="$1">
                        <Label htmlFor="gym-exercise-name" size="$2" color={palette.secondary}>
                            Name
                        </Label>
                        <Input
                            id="gym-exercise-name"
                            value={draft.name}
                            onChangeText={(name) => onChange({ ...draft, name })}
                            placeholder="Name"
                        />
                    </YStack>
                    <Muted>Share of the work each muscle does, 1 for the main mover.</Muted>
                    <XStack gap="$3" flexWrap="wrap">
                        {muscleGroups.map(({ group, label }) => (
                            <YStack key={group} grow={1} flexBasis={300}>
                                <GymCard>
                                    <Text fontWeight="600">{label}</Text>
                                    {muscles
                                        .filter((muscle) => muscle.group === group)
                                        .map((muscle) => (
                                            <Stepper
                                                key={muscle.muscle}
                                                label={muscle.display_name}
                                                value={
                                                    draft.muscles.find(
                                                        (target) => target.muscle === muscle.muscle,
                                                    )?.share ?? 0
                                                }
                                                step={0.1}
                                                min={0}
                                                max={1}
                                                format={(share) =>
                                                    share === 0 ? "–" : share.toFixed(1)
                                                }
                                                onChange={(share) => setShare(muscle, share)}
                                            />
                                        ))}
                                </GymCard>
                            </YStack>
                        ))}
                    </XStack>
                    <XStack items="center" gap="$3">
                        <Switch
                            id="gym-exercise-archived"
                            size="$3"
                            checked={draft.archived}
                            onCheckedChange={(archived) => onChange({ ...draft, archived })}
                        >
                            <Switch.Thumb />
                        </Switch>
                        <Label htmlFor="gym-exercise-archived">
                            Archived (hidden when picking exercises)
                        </Label>
                    </XStack>
                    <XStack gap="$2" justify="flex-end">
                        <Button busy={busy} onPress={onClose}>
                            Cancel
                        </Button>
                        <Button
                            tone="accent"
                            busy={busy}
                            disabled={!draft.name.trim()}
                            onPress={() => onSave(draft)}
                        >
                            Save exercise
                        </Button>
                    </XStack>
                </>
            )}
        </GymDialog>
    );
}

function TemplateEditor({
    draft,
    exercises,
    backend,
    busy,
    onError,
    onChange,
    onClose,
    onSave,
    onDelete,
}: {
    draft: TemplateDraft | null;
    exercises: GymExercise[];
    backend: GymBackend;
    busy: boolean;
    onError: ErrorHandler;
    onChange: (draft: TemplateDraft) => void;
    onClose: () => void;
    onSave: (draft: TemplateDraft) => void;
    onDelete: (draft: TemplateDraft) => void;
}) {
    const [picking, setPicking] = useState(false);
    const names = new Map(exercises.map((exercise) => [exercise.id, exercise.name]));

    function update(index: number, change: Partial<GymPlannedExercise>) {
        if (!draft) return;
        onChange({
            ...draft,
            exercises: draft.exercises.map((planned, position) =>
                position === index ? { ...planned, ...change } : planned,
            ),
        });
    }

    function move(index: number, offset: number) {
        if (!draft) return;
        const current = draft;
        const target = index + offset;
        if (target < 0 || target >= current.exercises.length) return;
        backend.moveTemplateExercise(current.exercises, index, target).then(
            (moved) => onChange({ ...current, exercises: moved }),
            (caught: unknown) => onError(formatBackendError(caught)),
        );
    }

    function remove(index: number) {
        if (!draft) return;
        const removed = draft.exercises[index];
        onChange({
            ...draft,
            exercises: draft.exercises
                .filter((_, position) => position !== index)
                .map((other, position) =>
                    position === 0 || (position === index && !removed?.superset_with_previous)
                        ? Object.assign({}, other, { superset_with_previous: false })
                        : other,
                ),
        });
    }

    return (
        <GymDialog
            open={draft !== null}
            title={draft?.id ? "Edit workout" : "New workout"}
            onClose={onClose}
            busy={busy}
            width={820}
        >
            {draft && (
                <>
                    <YStack gap="$1">
                        <Label htmlFor="gym-workout-name" size="$2" color={palette.secondary}>
                            Name
                        </Label>
                        <Input
                            id="gym-workout-name"
                            value={draft.name}
                            onChangeText={(name) => onChange({ ...draft, name })}
                            placeholder="Name"
                        />
                    </YStack>
                    {draft.exercises.length === 0 && (
                        <Muted>Add the exercises in the order you do them.</Muted>
                    )}
                    {draft.exercises.map((planned, index) => (
                        <GymCard key={planned.exercise_id}>
                            <XStack items="center" gap="$2">
                                <Text flex={1} fontWeight="600">
                                    {`${index + 1}. ${names.get(planned.exercise_id) ?? "Unknown exercise"}`}
                                </Text>
                                <Button
                                    size="$2"
                                    aria-label="Move up"
                                    icon={<ChevronUp size={16} />}
                                    disabled={index === 0}
                                    onPress={() => move(index, -1)}
                                />
                                <Button
                                    size="$2"
                                    aria-label="Move down"
                                    icon={<ChevronDown size={16} />}
                                    disabled={index === draft.exercises.length - 1}
                                    onPress={() => move(index, 1)}
                                />
                                <Button
                                    size="$2"
                                    aria-label="Remove exercise"
                                    icon={<Trash2 size={14} />}
                                    onPress={() => remove(index)}
                                />
                            </XStack>
                            <XStack gap="$5" flexWrap="wrap">
                                <YStack grow={1} flexBasis={300} gap="$2">
                                    <Stepper
                                        label="Working sets"
                                        value={planned.target_sets}
                                        min={1}
                                        max={20}
                                        onChange={(target_sets) => update(index, { target_sets })}
                                    />
                                    <Stepper
                                        label="Reps"
                                        value={planned.target_reps}
                                        min={1}
                                        max={100}
                                        onChange={(target_reps) => update(index, { target_reps })}
                                    />
                                    <Stepper
                                        label="Reps in reserve"
                                        value={planned.target_reps_in_reserve ?? 2}
                                        max={5}
                                        onChange={(target_reps_in_reserve) =>
                                            update(index, { target_reps_in_reserve })
                                        }
                                    />
                                </YStack>
                                <YStack grow={1} flexBasis={300} gap="$2">
                                    <Stepper
                                        label="Rest"
                                        value={planned.rest_seconds}
                                        step={15}
                                        max={600}
                                        format={formatRestSeconds}
                                        onChange={(rest_seconds) => update(index, { rest_seconds })}
                                    />
                                    <Stepper
                                        label="Warm-up sets"
                                        value={planned.warm_up_sets}
                                        max={10}
                                        onChange={(warm_up_sets) => update(index, { warm_up_sets })}
                                    />
                                    {planned.warm_up_sets > 0 && (
                                        <Stepper
                                            label="Warm-up rest"
                                            value={planned.warm_up_rest_seconds}
                                            step={15}
                                            max={600}
                                            format={formatRestSeconds}
                                            onChange={(warm_up_rest_seconds) =>
                                                update(index, { warm_up_rest_seconds })
                                            }
                                        />
                                    )}
                                </YStack>
                            </XStack>
                            {index > 0 && (
                                <XStack items="center" gap="$3">
                                    <Switch
                                        id={`gym-superset-${planned.exercise_id}`}
                                        size="$2"
                                        checked={planned.superset_with_previous}
                                        onCheckedChange={(superset_with_previous) =>
                                            update(index, { superset_with_previous })
                                        }
                                    >
                                        <Switch.Thumb />
                                    </Switch>
                                    <Label
                                        htmlFor={`gym-superset-${planned.exercise_id}`}
                                        size="$2"
                                        color={palette.secondary}
                                    >
                                        Superset with the exercise above
                                    </Label>
                                </XStack>
                            )}
                        </GymCard>
                    ))}
                    <XStack>
                        <Button icon={<Plus size={16} />} onPress={() => setPicking(true)}>
                            Add exercise
                        </Button>
                    </XStack>
                    <XStack gap="$2" justify="space-between" flexWrap="wrap">
                        {draft.id ? (
                            <Button tone="danger" busy={busy} onPress={() => onDelete(draft)}>
                                Delete workout
                            </Button>
                        ) : (
                            <YStack />
                        )}
                        <XStack gap="$2">
                            <Button busy={busy} onPress={onClose}>
                                Cancel
                            </Button>
                            <Button
                                tone="accent"
                                busy={busy}
                                disabled={!draft.name.trim() || draft.exercises.length === 0}
                                onPress={() => onSave(draft)}
                            >
                                Save workout
                            </Button>
                        </XStack>
                    </XStack>
                    <ExercisePicker
                        open={picking}
                        exercises={exercises}
                        excluded={new Set(draft.exercises.map((planned) => planned.exercise_id))}
                        onClose={() => setPicking(false)}
                        onPick={(exercise) => {
                            setPicking(false);
                            onChange({
                                ...draft,
                                exercises: [
                                    ...draft.exercises,
                                    {
                                        exercise_id: exercise.id,
                                        warm_up_sets: 0,
                                        warm_up_rest_seconds: 60,
                                        target_sets: 3,
                                        target_reps: 10,
                                        target_reps_in_reserve: 2,
                                        rest_seconds: 120,
                                        superset_with_previous: false,
                                    },
                                ],
                            });
                        }}
                    />
                </>
            )}
        </GymDialog>
    );
}
