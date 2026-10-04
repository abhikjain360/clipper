import { ChevronDown, ChevronUp, Plus, Trash2 } from "lucide-react-native";
import { useCallback, useEffect, useRef, useState } from "react";
import { Alert } from "react-native";
import { Button, H2, Input, ScrollView, Switch, Text, XStack, YStack } from "tamagui";
import {
  MuscleGroup,
  type GymExercise,
  type GymMuscleInfo,
  type GymMuscleShare,
  type GymPlannedExercise,
  type GymTemplate,
} from "@clipper/mobile-bridge";
import { formatBackendError } from "../backend";
import { formatRestSeconds, gym } from "./gymClient";
import { colors, ExercisePicker, GymCard, Muted, SheetModal, Stepper } from "./GymUi";

type ExerciseDraft = {
  id: string | undefined;
  name: string;
  muscles: GymMuscleShare[];
  archived: boolean;
};
type TemplateDraft = { id: string | undefined; name: string; exercises: GymPlannedExercise[] };

const groupLabels: { group: MuscleGroup; label: string }[] = [
  { group: MuscleGroup.Push, label: "Push" },
  { group: MuscleGroup.Pull, label: "Pull" },
  { group: MuscleGroup.Legs, label: "Legs" },
  { group: MuscleGroup.Core, label: "Core" },
];

export function Library({ onError }: { onError: (error: string | null) => void }) {
  const [tab, setTab] = useState<"exercises" | "workouts">("workouts");
  const [exercises, setExercises] = useState<GymExercise[]>([]);
  const [templates, setTemplates] = useState<GymTemplate[]>([]);
  const [exerciseDraft, setExerciseDraft] = useState<ExerciseDraft | null>(null);
  const [templateDraft, setTemplateDraft] = useState<TemplateDraft | null>(null);
  const busyRef = useRef(false);

  const load = useCallback(async () => {
    try {
      const [exerciseList, templateList] = await Promise.all([
        gym().gymExercises(),
        gym().gymTemplates(),
      ]);
      setExercises(exerciseList);
      setTemplates(templateList);
    } catch (caught) {
      onError(formatBackendError(caught));
    }
  }, [onError]);

  useEffect(() => {
    void load();
  }, [load]);

  async function run(action: () => Promise<unknown>): Promise<boolean> {
    if (busyRef.current) return false;
    busyRef.current = true;
    onError(null);
    try {
      await action();
      return true;
    } catch (caught) {
      onError(formatBackendError(caught));
      return false;
    } finally {
      busyRef.current = false;
      await load();
    }
  }

  const names = new Map(exercises.map((exercise) => [exercise.id, exercise.name]));
  const active = exercises.filter((exercise) => !exercise.archived);
  const archived = exercises.filter((exercise) => exercise.archived);

  return (
    <ScrollView flex={1} keyboardShouldPersistTaps="always">
      <YStack gap="$3" pb="$8">
        <XStack gap="$2">
          <Button
            flex={1}
            theme={tab === "workouts" ? "blue" : undefined}
            onPress={() => setTab("workouts")}
          >
            Workouts
          </Button>
          <Button
            flex={1}
            theme={tab === "exercises" ? "blue" : undefined}
            onPress={() => setTab("exercises")}
          >
            Exercises
          </Button>
        </XStack>

        {tab === "workouts" ? (
          <YStack gap="$2">
            <XStack items="center" justify="space-between">
              <H2 size="$6">Workouts</H2>
              <Button
                size="$3"
                icon={<Plus size={16} />}
                onPress={() => setTemplateDraft({ id: undefined, name: "", exercises: [] })}
              >
                New workout
              </Button>
            </XStack>
            {templates.length === 0 && <Muted>No workouts yet</Muted>}
            {templates.map((template) => (
              <XStack
                key={template.id}
                onPress={() =>
                  setTemplateDraft({
                    id: template.id,
                    name: template.name,
                    exercises: template.exercises.map((planned) => ({ ...planned })),
                  })
                }
                pressStyle={{ opacity: 0.6 }}
              >
                <YStack flex={1}>
                  <GymCard>
                    <YStack gap="$1">
                      <Text fontWeight="600">{template.name}</Text>
                      <Muted>
                        {template.exercises
                          .map((planned) => names.get(planned.exerciseId) ?? "Unknown exercise")
                          .join(", ")}
                      </Muted>
                    </YStack>
                  </GymCard>
                </YStack>
              </XStack>
            ))}
          </YStack>
        ) : (
          <YStack gap="$2">
            <XStack items="center" justify="space-between">
              <H2 size="$6">Exercises</H2>
              <Button
                size="$3"
                icon={<Plus size={16} />}
                onPress={() =>
                  setExerciseDraft({ id: undefined, name: "", muscles: [], archived: false })
                }
              >
                New exercise
              </Button>
            </XStack>
            {[...active, ...archived].map((exercise) => (
              <XStack
                key={exercise.id}
                items="center"
                justify="space-between"
                px="$3"
                py="$3"
                rounded="$3"
                bg={colors.card}
                onPress={() =>
                  setExerciseDraft({
                    id: exercise.id,
                    name: exercise.name,
                    muscles: exercise.muscles.map((share) => ({ ...share })),
                    archived: exercise.archived,
                  })
                }
                pressStyle={{ opacity: 0.6 }}
              >
                <Text color={exercise.archived ? colors.faint : undefined}>{exercise.name}</Text>
                {exercise.archived && <Muted>archived</Muted>}
              </XStack>
            ))}
          </YStack>
        )}
      </YStack>

      <ExerciseEditor
        draft={exerciseDraft}
        onChange={setExerciseDraft}
        onClose={() => setExerciseDraft(null)}
        onSave={(draft) =>
          void run(() =>
            gym().gymSaveExercise(draft.id, {
              name: draft.name,
              muscles: draft.muscles,
              archived: draft.archived,
            }),
          ).then((saved) => saved && setExerciseDraft(null))
        }
      />
      <TemplateEditor
        draft={templateDraft}
        exercises={exercises}
        onChange={setTemplateDraft}
        onClose={() => setTemplateDraft(null)}
        onSave={(draft) =>
          void run(() => gym().gymSaveTemplate(draft.id, draft.name, draft.exercises)).then(
            (saved) => saved && setTemplateDraft(null),
          )
        }
        onDelete={(draft) => {
          const id = draft.id;
          if (!id) return;
          Alert.alert(`Delete ${draft.name}?`, "Past workouts keep their sets.", [
            { text: "Cancel", style: "cancel" },
            {
              text: "Delete",
              style: "destructive",
              onPress: () =>
                void run(() => gym().gymDeleteTemplate(id)).then(
                  (deleted) => deleted && setTemplateDraft(null),
                ),
            },
          ]);
        }}
      />
    </ScrollView>
  );
}

function ExerciseEditor({
  draft,
  onChange,
  onClose,
  onSave,
}: {
  draft: ExerciseDraft | null;
  onChange: (draft: ExerciseDraft) => void;
  onClose: () => void;
  onSave: (draft: ExerciseDraft) => void;
}) {
  const [muscles] = useState<GymMuscleInfo[]>(() => gym().gymMuscles());
  if (!draft)
    return (
      <SheetModal visible={false} title="" onClose={onClose}>
        {null}
      </SheetModal>
    );

  function setShare(muscle: GymMuscleInfo, share: number) {
    if (!draft) return;
    const others = draft.muscles.filter((target) => target.muscle !== muscle.muscle);
    onChange({
      ...draft,
      muscles: share > 0 ? [...others, { muscle: muscle.muscle, share }] : others,
    });
  }

  return (
    <SheetModal visible title={draft.id ? "Edit exercise" : "New exercise"} onClose={onClose}>
      <Input
        value={draft.name}
        onChangeText={(name) => onChange({ ...draft, name })}
        placeholder="Name"
      />
      <Muted>Share of the work each muscle does, 1 for the main mover.</Muted>
      {groupLabels.map(({ group, label }) => (
        <GymCard key={label}>
          <YStack gap="$2">
            <Text fontWeight="600">{label}</Text>
            {muscles
              .filter((muscle) => muscle.group === group)
              .map((muscle) => (
                <Stepper
                  key={muscle.displayName}
                  label={muscle.displayName}
                  value={
                    draft.muscles.find((target) => target.muscle === muscle.muscle)?.share ?? 0
                  }
                  step={0.1}
                  min={0}
                  max={1}
                  format={(share) => (share === 0 ? "–" : share.toFixed(1))}
                  onChange={(share) => setShare(muscle, share)}
                />
              ))}
          </YStack>
        </GymCard>
      ))}
      <XStack items="center" justify="space-between">
        <Text>Archived</Text>
        <Switch
          checked={draft.archived}
          onCheckedChange={(archived) => onChange({ ...draft, archived })}
        >
          <Switch.Thumb />
        </Switch>
      </XStack>
      <Button theme="blue" size="$5" disabled={!draft.name.trim()} onPress={() => onSave(draft)}>
        Save exercise
      </Button>
    </SheetModal>
  );
}

function TemplateEditor({
  draft,
  exercises,
  onChange,
  onClose,
  onSave,
  onDelete,
}: {
  draft: TemplateDraft | null;
  exercises: GymExercise[];
  onChange: (draft: TemplateDraft) => void;
  onClose: () => void;
  onSave: (draft: TemplateDraft) => void;
  onDelete: (draft: TemplateDraft) => void;
}) {
  const [picking, setPicking] = useState(false);
  if (!draft)
    return (
      <SheetModal visible={false} title="" onClose={onClose}>
        {null}
      </SheetModal>
    );
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
    const target = index + offset;
    if (target < 0 || target >= draft.exercises.length) return;
    onChange({
      ...draft,
      exercises: gym().gymMoveTemplateExercise(draft.exercises, index, target),
    });
  }

  function remove(index: number) {
    if (!draft) return;
    const removed = draft.exercises[index];
    onChange({
      ...draft,
      exercises: draft.exercises
        .filter((_, position) => position !== index)
        .map((other, position) =>
          position === 0 || (position === index && !removed?.supersetWithPrevious)
            ? Object.assign({}, other, { supersetWithPrevious: false })
            : other,
        ),
    });
  }

  return (
    <SheetModal visible title={draft.id ? "Edit workout" : "New workout"} onClose={onClose}>
      <Input
        value={draft.name}
        onChangeText={(name) => onChange({ ...draft, name })}
        placeholder="Name"
      />
      {draft.exercises.length === 0 && <Muted>Add the exercises in the order you do them.</Muted>}
      {draft.exercises.map((planned, index) => (
        <GymCard key={planned.exerciseId}>
          <YStack gap="$2">
            <XStack items="center" gap="$2">
              <Text flex={1} fontWeight="600" numberOfLines={2}>
                {`${index + 1}. ${names.get(planned.exerciseId) ?? "Unknown exercise"}`}
              </Text>
              <Button
                size="$3"
                aria-label="Move up"
                icon={<ChevronUp size={16} />}
                disabled={index === 0}
                onPress={() => move(index, -1)}
              />
              <Button
                size="$3"
                aria-label="Move down"
                icon={<ChevronDown size={16} />}
                disabled={index === draft.exercises.length - 1}
                onPress={() => move(index, 1)}
              />
              <Button
                size="$3"
                aria-label="Remove exercise"
                icon={<Trash2 size={16} color={colors.bad} />}
                onPress={() => remove(index)}
              />
            </XStack>
            <Stepper
              label="Warm-up sets"
              value={planned.warmUpSets}
              max={10}
              onChange={(warmUpSets) => update(index, { warmUpSets })}
            />
            {planned.warmUpSets > 0 && (
              <Stepper
                label="Warm-up rest"
                value={planned.warmUpRestSeconds}
                step={15}
                max={600}
                format={formatRestSeconds}
                onChange={(warmUpRestSeconds) => update(index, { warmUpRestSeconds })}
              />
            )}
            <Stepper
              label="Working sets"
              value={planned.targetSets}
              min={1}
              max={20}
              onChange={(targetSets) => update(index, { targetSets })}
            />
            <Stepper
              label="Reps"
              value={planned.targetReps}
              min={1}
              max={100}
              onChange={(targetReps) => update(index, { targetReps })}
            />
            <Stepper
              label="Reps in reserve"
              value={planned.targetRepsInReserve ?? 2}
              max={5}
              onChange={(targetRepsInReserve) => update(index, { targetRepsInReserve })}
            />
            <Stepper
              label="Rest"
              value={planned.restSeconds}
              step={15}
              max={600}
              format={formatRestSeconds}
              onChange={(restSeconds) => update(index, { restSeconds })}
            />
            {index > 0 && (
              <XStack items="center" justify="space-between">
                <Text color={colors.muted} fontSize={13}>
                  Superset with the exercise above
                </Text>
                <Switch
                  size="$3"
                  checked={planned.supersetWithPrevious}
                  onCheckedChange={(supersetWithPrevious) =>
                    update(index, { supersetWithPrevious })
                  }
                >
                  <Switch.Thumb />
                </Switch>
              </XStack>
            )}
          </YStack>
        </GymCard>
      ))}
      <Button icon={<Plus size={16} />} onPress={() => setPicking(true)}>
        Add exercise
      </Button>
      <Button
        theme="blue"
        size="$5"
        disabled={!draft.name.trim() || draft.exercises.length === 0}
        onPress={() => onSave(draft)}
      >
        Save workout
      </Button>
      {draft.id && (
        <Button theme="red" onPress={() => onDelete(draft)}>
          Delete workout
        </Button>
      )}
      <ExercisePicker
        visible={picking}
        exercises={exercises}
        excluded={new Set(draft.exercises.map((planned) => planned.exerciseId))}
        onClose={() => setPicking(false)}
        onPick={(exercise) => {
          setPicking(false);
          onChange({
            ...draft,
            exercises: [
              ...draft.exercises,
              {
                exerciseId: exercise.id,
                warmUpSets: 0,
                warmUpRestSeconds: 60,
                targetSets: 3,
                targetReps: 10,
                targetRepsInReserve: 2,
                restSeconds: 120,
                supersetWithPrevious: false,
              },
            ],
          });
        }}
      />
    </SheetModal>
  );
}
