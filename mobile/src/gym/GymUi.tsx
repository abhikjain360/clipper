import { Input } from "../tamagui.config";
import { Button } from "./Button";
import { Minus, Plus, Search, X } from "lucide-react-native";
import { useContext, useEffect, useState, type ReactNode } from "react";
import { KeyboardAvoidingView, Modal, TextInput } from "react-native";
import { SafeAreaView } from "react-native-safe-area-context";
import Svg, { Circle, Line, Polyline, Text as SvgText } from "react-native-svg";
import { Card, H2, Paragraph, ScrollView, Text, XStack, YStack } from "tamagui";
import type { GymExercise, GymSet, GymSetValues } from "@clipper/mobile-bridge";
import { fatigueColors, formatKg, palette, parseCount, parseWeight } from "@clipper/shared";
import { setLabel } from "./gymClient";
import { GymError } from "./errors";

export const colors = {
  background: palette.pageFill,
  card: palette.cardFill,
  grid: palette.buttonFill,
  muted: palette.secondary,
  faint: palette.secondary,
  accent: palette.accent,
  good: palette.success,
  warm: palette.warning,
  bad: palette.danger,
  warmUp: fatigueColors.warmUp,
} as const;

export function GymCard({ children, highlighted }: { children: ReactNode; highlighted?: boolean }) {
  return (
    <Card
      p="$3"
      bg={highlighted ? palette.selectedFill : palette.cardFill}
      borderColor={palette.selectedBorder}
      borderWidth={highlighted ? 1 : 0}
    >
      {children}
    </Card>
  );
}

export function Muted({ children }: { children: ReactNode }) {
  return (
    <Paragraph size="$2" color={colors.muted}>
      {children}
    </Paragraph>
  );
}

export function NumberEntry({
  label,
  value,
  onChange,
  step,
  decimal,
  placeholder,
}: {
  label: string;
  value: string;
  onChange: (value: string) => void;
  step: number;
  decimal?: boolean;
  placeholder?: string;
}) {
  function nudge(direction: number) {
    const current = decimal ? (parseWeight(value) ?? 0) : (parseCount(value) ?? 0);
    const next = Math.max(0, Math.round((current + direction * step) * 100) / 100);
    onChange(next === 0 ? "" : String(next));
  }

  return (
    <YStack gap="$1">
      <Text color={colors.muted} fontSize={13}>
        {label}
      </Text>
      <XStack gap="$2" items="center">
        <Button
          size="$5"
          width={64}
          aria-label={`Less ${label}`}
          icon={<Minus size={22} />}
          onPress={() => nudge(-1)}
        />
        <TextInput
          value={value}
          onChangeText={onChange}
          keyboardType={decimal ? "decimal-pad" : "number-pad"}
          placeholder={placeholder}
          placeholderTextColor={colors.faint}
          selectTextOnFocus
          accessibilityLabel={label}
          style={{
            flex: 1,
            height: 56,
            borderRadius: 8,
            borderWidth: 0,
            backgroundColor: palette.inputFill,
            color: palette.text,
            fontSize: 26,
            textAlign: "center",
          }}
        />
        <Button
          size="$5"
          width={64}
          aria-label={`More ${label}`}
          icon={<Plus size={22} />}
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
  value: number | undefined;
  onChange: (value: number | undefined) => void;
}) {
  return (
    <YStack gap="$1">
      <Text color={colors.muted} fontSize={13}>
        Reps in reserve
      </Text>
      <XStack gap="$1.5">
        {[0, 1, 2, 3, 4, 5].map((reserve) => (
          <Button
            key={reserve}
            flex={1}
            size="$5"
            px={0}
            aria-label={`${reserve} reps in reserve`}
            selected={value === reserve}
            onPress={() => onChange(value === reserve ? undefined : reserve)}
          >
            {String(reserve)}
          </Button>
        ))}
      </XStack>
    </YStack>
  );
}

export function Stepper({
  busy = false,
  label,
  value,
  onChange,
  step = 1,
  min = 0,
  max,
  format,
}: {
  busy?: boolean;
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
      <Text color={colors.muted} fontSize={13} flex={1}>
        {label}
      </Text>
      <XStack items="center" gap="$2">
        <Button
          size="$3"
          busy={busy}
          aria-label={`Less ${label}`}
          icon={<Minus size={16} />}
          onPress={() => change(-1)}
        />
        <Text minW={56} style={{ textAlign: "center" }}>
          {format ? format(value) : String(value)}
        </Text>
        <Button
          size="$3"
          busy={busy}
          aria-label={`More ${label}`}
          icon={<Plus size={16} />}
          onPress={() => change(1)}
        />
      </XStack>
    </XStack>
  );
}

export function SheetModal({
  visible,
  title,
  onClose,
  children,
  busy = false,
}: {
  visible: boolean;
  title: string;
  onClose: () => void;
  children: ReactNode;
  busy?: boolean;
}) {
  const error = useContext(GymError);
  return (
    <Modal
      visible={visible}
      animationType="slide"
      onRequestClose={() => {
        if (!busy) onClose();
      }}
    >
      <SafeAreaView style={{ flex: 1, backgroundColor: colors.background }}>
        <XStack items="center" justify="space-between" gap="$2" px="$3" py="$2" borderWidth={0}>
          <H2 size="$5" flex={1} numberOfLines={1}>
            {title}
          </H2>
          <Button
            size="$3"
            aria-label="Close"
            icon={<X size={16} />}
            disabled={busy}
            onPress={onClose}
          />
        </XStack>
        <KeyboardAvoidingView style={{ flex: 1 }} behavior="padding">
          <ScrollView flex={1} keyboardShouldPersistTaps="always">
            <YStack gap="$3" p="$3" pb="$8">
              {error && (
                <Paragraph accessibilityRole="alert" color={colors.bad}>
                  {error}
                </Paragraph>
              )}
              {children}
            </YStack>
          </ScrollView>
        </KeyboardAvoidingView>
      </SafeAreaView>
    </Modal>
  );
}

export function ExercisePicker({
  visible,
  exercises,
  excluded,
  onPick,
  onClose,
  busy = false,
}: {
  visible: boolean;
  exercises: GymExercise[];
  excluded?: ReadonlySet<string>;
  onPick: (exercise: GymExercise) => void;
  onClose: () => void;
  busy?: boolean;
}) {
  const [search, setSearch] = useState("");
  useEffect(() => {
    if (visible) setSearch("");
  }, [visible]);
  const query = search.trim().toLowerCase();
  const shown = exercises.filter(
    (exercise) =>
      !exercise.archived &&
      !excluded?.has(exercise.id) &&
      exercise.name.toLowerCase().includes(query),
  );
  return (
    <SheetModal visible={visible} title="Pick an exercise" onClose={onClose} busy={busy}>
      <XStack items="center" gap="$2">
        <Search size={18} color={colors.muted} />
        <Input flex={1} value={search} onChangeText={setSearch} placeholder="Search" />
      </XStack>
      {shown.length === 0 && <Muted>No matching exercises</Muted>}
      {shown.map((exercise) => (
        <Button
          key={exercise.id}
          justify="flex-start"
          size="$5"
          busy={busy}
          onPress={() => onPick(exercise)}
        >
          {exercise.name}
        </Button>
      ))}
    </SheetModal>
  );
}

export function SetEditor({
  set,
  number,
  onSave,
  onDelete,
  onClose,
  busy = false,
}: {
  set: GymSet | null;
  number: number;
  onSave: (values: GymSetValues) => void;
  onDelete: () => void;
  onClose: () => void;
  busy?: boolean;
}) {
  const [weight, setWeight] = useState("");
  const [reps, setReps] = useState("");
  const [reserve, setReserve] = useState<number | undefined>(undefined);
  useEffect(() => {
    if (!set) return;
    setWeight(set.weightKg === undefined ? "" : formatKg(set.weightKg));
    setReps(set.reps === undefined ? "" : String(set.reps));
    setReserve(set.repsInReserve);
  }, [set]);
  return (
    <SheetModal
      visible={set !== null}
      title={set ? `Edit ${setLabel(set.kind, number).toLowerCase()}` : ""}
      onClose={onClose}
      busy={busy}
    >
      <NumberEntry label="Weight (kg)" value={weight} onChange={setWeight} step={2.5} decimal />
      <NumberEntry label="Reps" value={reps} onChange={setReps} step={1} />
      <ReserveChips value={reserve} onChange={setReserve} />
      <Button
        tone="accent"
        size="$5"
        busy={busy}
        onPress={() =>
          onSave({ weightKg: parseWeight(weight), reps: parseCount(reps), repsInReserve: reserve })
        }
      >
        Save set
      </Button>
      <Button tone="danger" size="$4" busy={busy} onPress={onDelete}>
        Delete set
      </Button>
    </SheetModal>
  );
}

export function LineChart({
  points,
  formatY,
  formatX,
  height = 180,
}: {
  points: { x: number; y: number }[];
  formatY: (value: number) => string;
  formatX: (value: number) => string;
  height?: number;
}) {
  const [width, setWidth] = useState(0);
  if (points.length === 0) return null;
  const left = 44;
  const right = 12;
  const top = 12;
  const bottom = 24;
  const xs = points.map((point) => point.x);
  const ys = points.map((point) => point.y);
  const minX = Math.min(...xs);
  const maxX = Math.max(...xs);
  const rawMinY = Math.min(...ys);
  const rawMaxY = Math.max(...ys);
  const padding = Math.max((rawMaxY - rawMinY) * 0.1, 0.5);
  const minY = rawMinY - padding;
  const maxY = rawMaxY + padding;
  const plotWidth = Math.max(width - left - right, 1);
  const plotHeight = height - top - bottom;
  const toX = (x: number) =>
    left + (maxX === minX ? plotWidth / 2 : ((x - minX) / (maxX - minX)) * plotWidth);
  const toY = (y: number) => top + (1 - (y - minY) / (maxY - minY)) * plotHeight;
  const line = points.map((point) => `${toX(point.x)},${toY(point.y)}`).join(" ");
  return (
    <YStack height={height} onLayout={(event) => setWidth(event.nativeEvent.layout.width)}>
      {width > 0 && (
        <Svg width={width} height={height}>
          {[rawMinY, rawMaxY].map((value, index) => (
            <Line
              key={`grid-${index}`}
              x1={left}
              x2={width - right}
              y1={toY(value)}
              y2={toY(value)}
              stroke={colors.grid}
              strokeWidth={1}
            />
          ))}
          <SvgText x={4} y={toY(rawMaxY) + 4} fill={colors.muted} fontSize={11}>
            {formatY(rawMaxY)}
          </SvgText>
          {rawMinY !== rawMaxY && (
            <SvgText x={4} y={toY(rawMinY) + 4} fill={colors.muted} fontSize={11}>
              {formatY(rawMinY)}
            </SvgText>
          )}
          <SvgText x={left} y={height - 6} fill={colors.muted} fontSize={11}>
            {formatX(minX)}
          </SvgText>
          {maxX !== minX && (
            <SvgText
              x={width - right}
              y={height - 6}
              fill={colors.muted}
              fontSize={11}
              textAnchor="end"
            >
              {formatX(maxX)}
            </SvgText>
          )}
          <Polyline points={line} fill="none" stroke={colors.accent} strokeWidth={2} />
          {points.map((point, index) => (
            <Circle
              key={`point-${index}`}
              cx={toX(point.x)}
              cy={toY(point.y)}
              r={3.5}
              fill={colors.accent}
            />
          ))}
        </Svg>
      )}
    </YStack>
  );
}
