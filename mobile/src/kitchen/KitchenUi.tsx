import { Button } from "../tamagui.config";
import { palette } from "@clipper/shared";
import { Check, Square } from "lucide-react-native";
import { useEffect, type ReactNode } from "react";
import { BackHandler, TextInput } from "react-native";
import { Spinner, Text, XStack, YStack } from "tamagui";
import { colors, Muted } from "../gym/GymUi";

export function Tick({
  checked,
  label,
  disabled,
  onPress,
}: {
  checked: boolean;
  label: string;
  disabled: boolean;
  onPress: () => void;
}) {
  return (
    <Button
      size="$4"
      width={48}
      px={0}
      aria-label={label}
      accessibilityRole="checkbox"
      accessibilityState={{ checked, disabled }}
      icon={checked ? <Check size={20} color={colors.good} /> : <Square size={20} />}
      disabled={disabled}
      onPress={onPress}
    />
  );
}

export function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <YStack gap="$1">
      <Text color={colors.muted}>{label}</Text>
      {children}
    </YStack>
  );
}

export function NotesInput({
  value,
  onChange,
  disabled,
}: {
  value: string;
  onChange: (value: string) => void;
  disabled: boolean;
}) {
  return (
    <TextInput
      accessibilityLabel="Notes"
      value={value}
      onChangeText={onChange}
      multiline
      style={{
        height: 140,
        textAlignVertical: "top",
        borderRadius: 8,
        borderWidth: 0,
        backgroundColor: palette.inputFill,
        color: palette.text,
        padding: 12,
        fontSize: 16,
      }}
      editable={!disabled}
      placeholder="Notes"
      placeholderTextColor={colors.faint}
    />
  );
}

export function LoadStatus({
  loading,
  failed,
  onRetry,
}: {
  loading: boolean;
  failed: boolean;
  onRetry: () => void;
}) {
  return (
    <XStack items="center" gap="$2">
      {loading && <Spinner size="small" />}
      {failed && !loading && (
        <>
          <Muted>Could not load. Try again.</Muted>
          <Button size="$3" onPress={onRetry}>
            Retry
          </Button>
        </>
      )}
    </XStack>
  );
}

export function useBack(onBack: () => void) {
  useEffect(() => {
    const subscription = BackHandler.addEventListener("hardwareBackPress", () => {
      onBack();
      return true;
    });
    return () => subscription.remove();
  }, [onBack]);
}
