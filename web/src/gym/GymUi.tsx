import { Minus, Plus, Search } from "lucide-react";
import { useCallback, useEffect, useId, useRef, useState, type ReactNode } from "react";
import { Card, Dialog, H2, Label, Paragraph, ScrollView, Text, XStack, YStack } from "tamagui";
import {
    fatigueColors,
    formatKg,
    formatSetLabel,
    formatSetValues,
    palette,
    parseCount,
    parseWeight,
    type GymExercise,
    type GymMuscleGroup,
    type GymSet,
    type GymSetKind,
    type GymSetValues,
} from "@clipper/shared";
import { Button, Input } from "../tamagui.config";
import { formatBackendError } from "../backend";

export type ErrorHandler = (error: string | null) => void;

export const muscleGroups: { group: GymMuscleGroup; label: string }[] = [
    { group: "push", label: "Push" },
    { group: "pull", label: "Pull" },
    { group: "legs", label: "Legs" },
    { group: "core", label: "Core" },
];

export function formatSet(set: {
    weight_kg: number | null;
    reps: number | null;
    reps_in_reserve: number | null;
}): string {
    return formatSetValues(set.weight_kg, set.reps, set.reps_in_reserve);
}

export function setLabel(kind: GymSetKind, number: number): string {
    return formatSetLabel(kind === "warm_up", number);
}

export function numberedSets(sets: GymSet[]): { set: GymSet; number: number }[] {
    const numbers = new Map<GymSetKind, number>();
    return sets.map((set) => {
        const number = (numbers.get(set.kind) ?? 0) + 1;
        numbers.set(set.kind, number);
        return { set, number };
    });
}

export function useGymData<T>(load: () => Promise<T>, state: unknown, onError: ErrorHandler) {
    const [value, setValue] = useState<T | undefined>(undefined);
    const [failed, setFailed] = useState(false);
    const [version, setVersion] = useState(0);
    const reload = useCallback(() => setVersion((current) => current + 1), []);
    useEffect(() => {
        let cancelled = false;
        load().then(
            (result) => {
                if (cancelled) return;
                setValue(result);
                setFailed(false);
            },
            (caught: unknown) => {
                if (cancelled) return;
                setFailed(true);
                onError(formatBackendError(caught));
            },
        );
        return () => {
            cancelled = true;
        };
    }, [load, state, version, onError]);
    return { value, failed, reload };
}

export function useGymChange(reload: () => void, onError: ErrorHandler) {
    const [busy, setBusy] = useState(false);
    const busyRef = useRef(false);
    const run = useCallback(
        async (action: () => Promise<unknown>): Promise<boolean> => {
            if (busyRef.current) return false;
            busyRef.current = true;
            setBusy(true);
            onError(null);
            try {
                await action();
                return true;
            } catch (caught) {
                onError(formatBackendError(caught));
                return false;
            } finally {
                busyRef.current = false;
                setBusy(false);
                reload();
            }
        },
        [reload, onError],
    );
    return { busy, run };
}

export function GymCard({
    children,
    highlighted = false,
}: {
    children: ReactNode;
    highlighted?: boolean;
}) {
    return (
        <Card
            p="$4"
            gap="$3"
            bg={highlighted ? palette.selectedFill : palette.cardFill}
            borderWidth={highlighted ? 1 : 0}
            borderColor={palette.selectedBorder}
        >
            {children}
        </Card>
    );
}

export function CardTitle({ children }: { children: ReactNode }) {
    return <H2 size="$5">{children}</H2>;
}

export function Muted({ children }: { children: ReactNode }) {
    return (
        <Paragraph size="$3" color={palette.secondary}>
            {children}
        </Paragraph>
    );
}

export function Columns({ children }: { children: ReactNode }) {
    return (
        <XStack gap="$4" flexWrap="wrap" items="flex-start">
            {children}
        </XStack>
    );
}

export function Column({
    children,
    grow = 1,
    minWidth = 320,
}: {
    children: ReactNode;
    grow?: number;
    minWidth?: number;
}) {
    return (
        <YStack flex={grow} flexBasis={0} minW={minWidth} gap="$4">
            {children}
        </YStack>
    );
}

export function GymDialog({
    open,
    title,
    onClose,
    children,
    busy = false,
    width = 560,
}: {
    open: boolean;
    title: string;
    onClose: () => void;
    children: ReactNode;
    busy?: boolean;
    width?: number;
}) {
    return (
        <Dialog
            modal
            open={open}
            onOpenChange={(next) => {
                if (!next && !busy) onClose();
            }}
        >
            <Dialog.Portal>
                <Dialog.Overlay key="overlay" bg="rgba(0,0,0,0.65)" />
                <Dialog.Content
                    key="content"
                    width="90vw"
                    maxW={width}
                    maxH="90vh"
                    p="$4"
                    gap="$3"
                    bg={palette.cardFill}
                    borderWidth={0}
                    style={{ overflowY: "auto" }}
                >
                    <Dialog.Title size="$7">{title}</Dialog.Title>
                    {children}
                </Dialog.Content>
            </Dialog.Portal>
        </Dialog>
    );
}

export function ConfirmDialog({
    open,
    title,
    description,
    confirmLabel,
    destructive = true,
    busy,
    onConfirm,
    onCancel,
}: {
    open: boolean;
    title: string;
    description: string;
    confirmLabel: string;
    destructive?: boolean;
    busy: boolean;
    onConfirm: () => void;
    onCancel: () => void;
}) {
    return (
        <GymDialog open={open} title={title} onClose={onCancel} busy={busy} width={440}>
            <Paragraph>{description}</Paragraph>
            <XStack gap="$2" justify="flex-end">
                <Button disabled={busy} onPress={onCancel}>
                    Cancel
                </Button>
                <Button
                    tone={destructive ? "danger" : "accent"}
                    disabled={busy}
                    onPress={onConfirm}
                >
                    {confirmLabel}
                </Button>
            </XStack>
        </GymDialog>
    );
}

export function NumberField({
    label,
    value,
    onChange,
    step,
    decimal = false,
    placeholder,
    onSubmit,
}: {
    label: string;
    value: string;
    onChange: (value: string) => void;
    step: number;
    decimal?: boolean;
    placeholder?: string;
    onSubmit?: () => void;
}) {
    const id = useId();
    function nudge(direction: number) {
        const current = decimal ? (parseWeight(value) ?? 0) : (parseCount(value) ?? 0);
        const next = Math.max(0, Math.round((current + direction * step) * 100) / 100);
        onChange(next === 0 ? "" : String(next));
    }
    return (
        <YStack gap="$1">
            <Label htmlFor={id} size="$2" color={palette.secondary}>
                {label}
            </Label>
            <XStack gap="$2" items="center">
                <Button
                    size="$3"
                    aria-label={`Less ${label}`}
                    icon={<Minus size={16} />}
                    onPress={() => nudge(-1)}
                />
                <Input
                    id={id}
                    width={96}
                    size="$4"
                    value={value}
                    onChangeText={onChange}
                    onSubmitEditing={onSubmit}
                    inputMode={decimal ? "decimal" : "numeric"}
                    placeholder={placeholder}
                    selectTextOnFocus
                    style={{ textAlign: "center" }}
                />
                <Button
                    size="$3"
                    aria-label={`More ${label}`}
                    icon={<Plus size={16} />}
                    onPress={() => nudge(1)}
                />
            </XStack>
        </YStack>
    );
}

export function ReserveChips({
    value,
    onChange,
}: {
    value: number | null;
    onChange: (value: number | null) => void;
}) {
    return (
        <YStack gap="$1">
            <Text fontSize={13} color={palette.secondary}>
                Reps in reserve
            </Text>
            <XStack gap="$1.5" role="group" aria-label="Reps in reserve">
                {[0, 1, 2, 3, 4, 5].map((reserve) => (
                    <Button
                        key={reserve}
                        size="$3"
                        width={44}
                        px={0}
                        aria-label={`${reserve} reps in reserve`}
                        aria-pressed={value === reserve}
                        selected={value === reserve}
                        onPress={() => onChange(value === reserve ? null : reserve)}
                    >
                        {String(reserve)}
                    </Button>
                ))}
            </XStack>
        </YStack>
    );
}

export function Stepper({
    label,
    value,
    onChange,
    step = 1,
    min = 0,
    max,
    format,
}: {
    label: string;
    value: number;
    onChange: (value: number) => void;
    step?: number;
    min?: number;
    max?: number;
    format?: (value: number) => string;
}) {
    function change(direction: number) {
        const next = Math.round((value + direction * step) * 100) / 100;
        if (next < min || (max !== undefined && next > max)) return;
        onChange(next);
    }
    return (
        <XStack items="center" justify="space-between" gap="$2">
            <Text fontSize={13} color={palette.secondary} flex={1}>
                {label}
            </Text>
            <XStack items="center" gap="$2">
                <Button
                    size="$2"
                    aria-label={`Less ${label}`}
                    icon={<Minus size={14} />}
                    onPress={() => change(-1)}
                />
                <Text minW={52} style={{ textAlign: "center" }}>
                    {format ? format(value) : String(value)}
                </Text>
                <Button
                    size="$2"
                    aria-label={`More ${label}`}
                    icon={<Plus size={14} />}
                    onPress={() => change(1)}
                />
            </XStack>
        </XStack>
    );
}

export type SetDraft = { weight: string; reps: string; reserve: number | null };

export function setDraft(values: GymSetValues | null | undefined): SetDraft {
    return {
        weight:
            values?.weight_kg === null || values?.weight_kg === undefined
                ? ""
                : formatKg(values.weight_kg),
        reps: values?.reps === null || values?.reps === undefined ? "" : String(values.reps),
        reserve: values?.reps_in_reserve ?? null,
    };
}

export function setValues(draft: SetDraft): GymSetValues {
    return {
        weight_kg: parseWeight(draft.weight) ?? null,
        reps: parseCount(draft.reps) ?? null,
        reps_in_reserve: draft.reserve,
    };
}

export function SetFields({
    draft,
    onChange,
    onSubmit,
}: {
    draft: SetDraft;
    onChange: (draft: SetDraft) => void;
    onSubmit?: () => void;
}) {
    return (
        <XStack gap="$4" flexWrap="wrap" items="flex-end">
            <NumberField
                label="Weight (kg)"
                value={draft.weight}
                onChange={(weight) => onChange({ ...draft, weight })}
                step={2.5}
                decimal
                placeholder="BW"
                onSubmit={onSubmit}
            />
            <NumberField
                label="Reps"
                value={draft.reps}
                onChange={(reps) => onChange({ ...draft, reps })}
                step={1}
                placeholder="–"
                onSubmit={onSubmit}
            />
            <ReserveChips
                value={draft.reserve}
                onChange={(reserve) => onChange({ ...draft, reserve })}
            />
        </XStack>
    );
}

export function SetEditor({
    editing,
    busy,
    onSave,
    onDelete,
    onClose,
}: {
    editing: { set: GymSet; number: number; exerciseName: string } | null;
    busy: boolean;
    onSave: (values: GymSetValues) => void;
    onDelete: () => void;
    onClose: () => void;
}) {
    const [draft, setDraftState] = useState<SetDraft>(setDraft(null));
    const set = editing?.set;
    useEffect(() => {
        if (set) setDraftState(setDraft(set));
    }, [set]);
    return (
        <GymDialog
            open={editing !== null}
            title={
                editing
                    ? `${editing.exerciseName}: ${setLabel(editing.set.kind, editing.number).toLowerCase()}`
                    : ""
            }
            onClose={onClose}
            busy={busy}
        >
            <SetFields
                draft={draft}
                onChange={setDraftState}
                onSubmit={() => onSave(setValues(draft))}
            />
            <XStack gap="$2" justify="space-between" flexWrap="wrap">
                <Button tone="danger" disabled={busy} onPress={onDelete}>
                    Delete set
                </Button>
                <XStack gap="$2">
                    <Button disabled={busy} onPress={onClose}>
                        Cancel
                    </Button>
                    <Button tone="accent" disabled={busy} onPress={() => onSave(setValues(draft))}>
                        Save set
                    </Button>
                </XStack>
            </XStack>
        </GymDialog>
    );
}

export function ExercisePicker({
    open,
    exercises,
    excluded,
    onPick,
    onClose,
}: {
    open: boolean;
    exercises: GymExercise[];
    excluded?: ReadonlySet<string>;
    onPick: (exercise: GymExercise) => void;
    onClose: () => void;
}) {
    const [search, setSearch] = useState("");
    useEffect(() => {
        if (open) setSearch("");
    }, [open]);
    const query = search.trim().toLowerCase();
    const shown = exercises.filter(
        (exercise) =>
            !exercise.archived &&
            !excluded?.has(exercise.id) &&
            exercise.name.toLowerCase().includes(query),
    );
    return (
        <GymDialog open={open} title="Pick an exercise" onClose={onClose}>
            <XStack items="center" gap="$2">
                <Search size={18} />
                <Input
                    flex={1}
                    value={search}
                    onChangeText={setSearch}
                    placeholder="Search exercises"
                    aria-label="Search exercises"
                    autoFocus
                />
            </XStack>
            <ScrollView maxH={420}>
                <YStack gap="$1">
                    {shown.length === 0 && <Muted>No matching exercises</Muted>}
                    {shown.map((exercise) => (
                        <Button
                            key={exercise.id}
                            justify="flex-start"
                            chromeless
                            onPress={() => onPick(exercise)}
                        >
                            {exercise.name}
                        </Button>
                    ))}
                </YStack>
            </ScrollView>
            <XStack justify="flex-end">
                <Button onPress={onClose}>Cancel</Button>
            </XStack>
        </GymDialog>
    );
}

function niceTicks(min: number, max: number): number[] {
    const rough = Math.max(max - min, 1e-6) / 3;
    const power = 10 ** Math.floor(Math.log10(rough));
    const step = [1, 2, 2.5, 5, 10].map((multiple) => multiple * power).find((s) => s >= rough);
    const size = step ?? power * 10;
    const ticks: number[] = [];
    for (let value = Math.ceil(min / size) * size; value <= max; value += size) {
        ticks.push(Math.round(value * 1000) / 1000);
    }
    return ticks;
}

export function LineChart({
    points,
    label,
    formatTick,
    formatValue,
    formatX,
    height = 240,
}: {
    points: { x: number; y: number }[];
    label: string;
    formatTick: (value: number) => string;
    formatValue: (value: number) => string;
    formatX: (value: number) => string;
    height?: number;
}) {
    const [width, setWidth] = useState(0);
    const [hovered, setHovered] = useState<number | null>(null);
    if (points.length === 0) return null;
    const left = 48;
    const right = 24;
    const top = 20;
    const bottom = 28;
    const xs = points.map((point) => point.x);
    const ys = points.map((point) => point.y);
    const minX = Math.min(...xs);
    const maxX = Math.max(...xs);
    const rawMinY = Math.min(...ys);
    const rawMaxY = Math.max(...ys);
    const padding = Math.max((rawMaxY - rawMinY) * 0.15, 0.5);
    const minY = rawMinY - padding;
    const maxY = rawMaxY + padding;
    const plotWidth = Math.max(width - left - right, 1);
    const plotHeight = height - top - bottom;
    const toX = (x: number) =>
        left + (maxX === minX ? plotWidth / 2 : ((x - minX) / (maxX - minX)) * plotWidth);
    const toY = (y: number) => top + (1 - (y - minY) / (maxY - minY)) * plotHeight;
    const accent = palette.accent;
    const grid = palette.buttonFill;
    const muted = palette.secondary;
    const surface = palette.cardFill;
    const last = points[points.length - 1];
    const active = hovered === null ? undefined : points[hovered];
    return (
        <YStack
            height={height}
            position="relative"
            onLayout={(event) => setWidth(event.nativeEvent.layout.width)}
        >
            {width > 0 && (
                <svg
                    width={width}
                    height={height}
                    role="img"
                    aria-label={label}
                    onPointerMove={(event) => {
                        const bounds = event.currentTarget.getBoundingClientRect();
                        const pointer = event.clientX - bounds.left;
                        let nearest = 0;
                        points.forEach((point, index) => {
                            const best = points[nearest];
                            if (
                                best &&
                                Math.abs(toX(point.x) - pointer) < Math.abs(toX(best.x) - pointer)
                            )
                                nearest = index;
                        });
                        setHovered(nearest);
                    }}
                    onPointerLeave={() => setHovered(null)}
                >
                    {niceTicks(minY, maxY).map((tick) => (
                        <g key={tick}>
                            <line
                                x1={left}
                                x2={width - right}
                                y1={toY(tick)}
                                y2={toY(tick)}
                                style={{ stroke: grid, strokeWidth: 1 }}
                            />
                            <text
                                x={left - 8}
                                y={toY(tick) + 4}
                                textAnchor="end"
                                fontSize={11}
                                style={{ fill: muted, fontVariantNumeric: "tabular-nums" }}
                            >
                                {formatTick(tick)}
                            </text>
                        </g>
                    ))}
                    <text x={left} y={height - 8} fontSize={11} style={{ fill: muted }}>
                        {formatX(minX)}
                    </text>
                    {maxX !== minX && (
                        <text
                            x={width - right}
                            y={height - 8}
                            fontSize={11}
                            textAnchor="end"
                            style={{ fill: muted }}
                        >
                            {formatX(maxX)}
                        </text>
                    )}
                    {active && (
                        <line
                            x1={toX(active.x)}
                            x2={toX(active.x)}
                            y1={top}
                            y2={top + plotHeight}
                            style={{ stroke: muted, strokeWidth: 1 }}
                        />
                    )}
                    <polyline
                        points={points.map((point) => `${toX(point.x)},${toY(point.y)}`).join(" ")}
                        fill="none"
                        style={{
                            stroke: accent,
                            strokeWidth: 2,
                            strokeLinejoin: "round",
                            strokeLinecap: "round",
                        }}
                    />
                    {points.map((point, index) => (
                        <circle
                            key={`${point.x}-${index}`}
                            cx={toX(point.x)}
                            cy={toY(point.y)}
                            r={index === hovered ? 6 : 4}
                            style={{ fill: accent, stroke: surface, strokeWidth: 2 }}
                        />
                    ))}
                    {last && hovered === null && (
                        <text
                            x={toX(last.x)}
                            y={toY(last.y) - 10}
                            textAnchor={points.length > 1 ? "end" : "middle"}
                            fontSize={12}
                            fontWeight={600}
                            style={{ fill: palette.text }}
                        >
                            {formatValue(last.y)}
                        </text>
                    )}
                </svg>
            )}
            {active && (
                <YStack
                    position="absolute"
                    t={0}
                    l={Math.min(Math.max(toX(active.x) - 60, 0), Math.max(width - 120, 0))}
                    width={120}
                    px="$2"
                    py="$1"
                    rounded="$3"
                    bg={palette.cardFill}
                    borderWidth={0}
                    pointerEvents="none"
                >
                    <Text fontWeight="600">{formatValue(active.y)}</Text>
                    <Text fontSize={12} color={palette.secondary}>
                        {formatX(active.x)}
                    </Text>
                </YStack>
            )}
        </YStack>
    );
}

export function SetRow({
    set,
    number,
    exerciseName,
    onEdit,
}: {
    set: GymSet;
    number: number;
    exerciseName: string;
    onEdit: () => void;
}) {
    return (
        <XStack
            items="center"
            justify="space-between"
            gap="$3"
            px="$3"
            py="$2"
            rounded="$3"
            bg={palette.pageFill}
            cursor="pointer"
            hoverStyle={{ bg: palette.cardFill }}
            pressStyle={{ bg: palette.cardFill }}
            role="button"
            aria-label={`Edit ${setLabel(set.kind, number).toLowerCase()} of ${exerciseName}`}
            onPress={onEdit}
        >
            <Text minW={90} color={set.kind === "warm_up" ? fatigueColors.warmUp : undefined}>
                {setLabel(set.kind, number)}
            </Text>
            <Text flex={1} style={{ fontVariantNumeric: "tabular-nums" }}>
                {formatSet(set)}
            </Text>
            <Text color={palette.secondary} fontSize={12}>
                {set.estimated_one_rep_max_kg === null || set.kind === "warm_up"
                    ? ""
                    : `e1RM ${Math.round(set.estimated_one_rep_max_kg * 10) / 10}`}
            </Text>
        </XStack>
    );
}
