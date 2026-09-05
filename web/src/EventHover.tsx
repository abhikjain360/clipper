import type { ReactNode } from "react";
import { Tooltip, Text, YStack } from "tamagui";

export function EventHover({
    title,
    detail,
    children,
}: {
    title: string;
    detail: string;
    children: ReactNode;
}) {
    return (
        <Tooltip delay={0} restMs={0} placement="bottom" allowFlip stayInFrame>
            <Tooltip.Trigger asChild>{children}</Tooltip.Trigger>
            <Tooltip.Content
                maxW={320}
                p="$3"
                bg="#222a33"
                borderWidth={1}
                borderColor="#526171"
                rounded="$3"
            >
                <Tooltip.Arrow bg="#222a33" borderWidth={1} borderColor="#526171" />
                <YStack gap="$1">
                    <Text fontSize={14} fontWeight="600">
                        {title || "Untitled"}
                    </Text>
                    <Text fontSize={13} color="#bac7d5">
                        {detail}
                    </Text>
                </YStack>
            </Tooltip.Content>
        </Tooltip>
    );
}
