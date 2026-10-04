import type { ReactNode } from "react";
import { Tooltip, Text, YStack } from "tamagui";
import { useEventHover } from "./calendar-hover";

export function EventHover({
    title,
    detail,
    children,
}: {
    title: string;
    detail: string;
    children: ReactNode;
}) {
    const hover = useEventHover();

    return (
        <Tooltip
            open={hover.open}
            onOpenChange={hover.onOpenChange}
            focus={{ enabled: false }}
            delay={0}
            restMs={0}
            placement="bottom"
            allowFlip
            stayInFrame
        >
            <Tooltip.Trigger
                asChild
                onPointerEnter={hover.onPointerEnter}
                onPointerLeave={hover.onPointerLeave}
                onPointerCancel={hover.onPointerCancel}
            >
                {children}
            </Tooltip.Trigger>
            <Tooltip.Content
                pointerEvents="none"
                width={300}
                maxW="calc(100vw - 24px)"
                p="$3"
                bg="#222a33"
                borderWidth={1}
                borderColor="#526171"
                rounded="$3"
            >
                <Tooltip.Arrow bg="#222a33" borderWidth={1} borderColor="#526171" />
                <YStack gap="$1" width="100%" minW={0}>
                    <Text
                        fontSize={14}
                        lineHeight={20}
                        fontWeight="600"
                        color="#f2f5f7"
                        style={{ overflowWrap: "anywhere" }}
                    >
                        {title || "Untitled"}
                    </Text>
                    <Text
                        fontSize={13}
                        lineHeight={18}
                        color="#bac7d5"
                        style={{ overflowWrap: "anywhere" }}
                    >
                        {detail}
                    </Text>
                </YStack>
            </Tooltip.Content>
        </Tooltip>
    );
}
