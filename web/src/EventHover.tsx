import { palette } from "@clipper/shared";
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
                bg={palette.raised}
                borderWidth={1}
                borderColor={palette.border}
                rounded="$3"
            >
                <Tooltip.Arrow bg={palette.raised} borderWidth={1} borderColor={palette.border} />
                <YStack gap="$1" width="100%" minW={0}>
                    <Text
                        fontSize={14}
                        lineHeight={20}
                        fontWeight="600"
                        color={palette.text}
                        style={{ overflowWrap: "anywhere" }}
                    >
                        {title || "Untitled"}
                    </Text>
                    <Text
                        fontSize={13}
                        lineHeight={18}
                        color={palette.secondary}
                        style={{ overflowWrap: "anywhere" }}
                    >
                        {detail}
                    </Text>
                </YStack>
            </Tooltip.Content>
        </Tooltip>
    );
}
