import { useEffect, useState } from "react";
import { Button, Paragraph, Text, XStack, YStack } from "tamagui";
import type { KitchenTimer, KitchenTimerAction } from "@clipper/shared";

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
        if (timer.state !== "running") return;
        const update = () => setNow(Date.now());
        update();
        const interval = setInterval(update, 1000);
        document.addEventListener("visibilitychange", update);
        return () => {
            clearInterval(interval);
            document.removeEventListener("visibilitychange", update);
        };
    }, [timer.state, timer.ends_at_millis]);
    const remaining =
        timer.state === "running"
            ? Math.max(0, (timer.ends_at_millis ?? now) - now)
            : (timer.remaining_millis ?? 0);
    const done = timer.state === "done" || (timer.state === "running" && remaining === 0);
    const seconds = Math.ceil(remaining / 1000);
    const countdown = `${Math.floor(seconds / 60)}:${String(seconds % 60).padStart(2, "0")}`;
    return (
        <YStack gap="$2" p="$2" bg="#101214" rounded="$2">
            <XStack gap="$2" items="center" flexWrap="wrap">
                <Text>
                    {timer.label} · {timer.minutes} min
                </Text>
                {timer.state !== "idle" && (
                    <Text
                        style={{ fontFamily: "monospace" }}
                        role="timer"
                        aria-label={`${timer.label} remaining`}
                    >
                        {done ? "Done" : countdown}
                        {timer.state === "paused" ? " · Paused" : ""}
                    </Text>
                )}
            </XStack>
            <Paragraph color="#9aa4ad" fontSize={12}>
                {timer.state === "idle"
                    ? "Starting here rings on this Mac."
                    : timer.rings_here
                      ? "Rings on this Mac."
                      : "Rings on another device."}
            </Paragraph>
            <XStack gap="$2" flexWrap="wrap">
                {timer.state === "idle" && (
                    <Button size="$2" disabled={disabled} onPress={() => onAction("start")}>
                        Start
                    </Button>
                )}
                {timer.state === "running" && !done && (
                    <Button size="$2" disabled={disabled} onPress={() => onAction("pause")}>
                        Pause
                    </Button>
                )}
                {timer.state === "paused" && (
                    <Button size="$2" disabled={disabled} onPress={() => onAction("resume")}>
                        Resume
                    </Button>
                )}
                {timer.state !== "idle" && (
                    <>
                        <Button
                            size="$2"
                            disabled={disabled}
                            onPress={() => onAction("add_minute")}
                        >
                            +1 min
                        </Button>
                        <Button size="$2" disabled={disabled} onPress={() => onAction("clear")}>
                            Clear
                        </Button>
                    </>
                )}
            </XStack>
        </YStack>
    );
}
