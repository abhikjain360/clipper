import { KitchenTimerAction, KitchenTimerState, type KitchenTimer } from "@clipper/mobile-bridge";
import { useEffect, useState } from "react";
import { AppState as NativeAppState } from "react-native";
import { Button, Text, XStack, YStack } from "tamagui";
import { colors, Muted } from "../gym/GymUi";
import { clock } from "./kitchenClient";

export function StepTimer({
  timer,
  disabled,
  onAction,
}: {
  timer: KitchenTimer;
  disabled: boolean;
  onAction: (action: KitchenTimerAction) => void;
}) {
  const [now, setNow] = useState(Date.now);

  useEffect(() => {
    if (timer.state !== KitchenTimerState.Running) return;
    setNow(Date.now());
    const interval = setInterval(() => setNow(Date.now()), 1000);
    const subscription = NativeAppState.addEventListener("change", (next) => {
      if (next === "active") setNow(Date.now());
    });
    return () => {
      clearInterval(interval);
      subscription.remove();
    };
  }, [timer.state, timer.endsAtMillis]);

  return (
    <YStack gap="$2" p="$2" bg="#1f2428" rounded="$2">
      <Text fontWeight="600">{`${timer.label} · ${timer.minutes} min`}</Text>
      {timer.state === KitchenTimerState.Running && timer.endsAtMillis !== undefined && (
        <Text fontSize={28} fontWeight="700" color={colors.accent} aria-label="Time remaining">
          {clock(timer.endsAtMillis - BigInt(now))}
        </Text>
      )}
      {timer.state === KitchenTimerState.Paused && timer.remainingMillis !== undefined && (
        <Text fontSize={24}>{`Paused · ${clock(timer.remainingMillis)}`}</Text>
      )}
      {timer.state === KitchenTimerState.Done && <Text color={colors.good}>Done</Text>}
      {timer.state !== KitchenTimerState.Idle && (
        <Muted>{timer.ringsHere ? "Rings on this phone" : "Rings on another device"}</Muted>
      )}
      <XStack gap="$2" flexWrap="wrap">
        {timer.state === KitchenTimerState.Idle && (
          <Button
            size="$4"
            theme="blue"
            disabled={disabled}
            onPress={() => onAction(KitchenTimerAction.Start)}
          >
            Start
          </Button>
        )}
        {timer.state === KitchenTimerState.Running && (
          <Button size="$4" disabled={disabled} onPress={() => onAction(KitchenTimerAction.Pause)}>
            Pause
          </Button>
        )}
        {timer.state === KitchenTimerState.Paused && (
          <Button size="$4" disabled={disabled} onPress={() => onAction(KitchenTimerAction.Resume)}>
            Resume
          </Button>
        )}
        {timer.state !== KitchenTimerState.Idle && (
          <>
            <Button
              size="$4"
              disabled={disabled}
              onPress={() => onAction(KitchenTimerAction.AddMinute)}
            >
              +1 min
            </Button>
            <Button
              size="$4"
              disabled={disabled}
              onPress={() => onAction(KitchenTimerAction.Clear)}
            >
              Clear
            </Button>
          </>
        )}
      </XStack>
    </YStack>
  );
}
