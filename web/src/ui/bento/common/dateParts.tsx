// Adapted from the free Tamagui Bento DatePicker, retrieved 2026-09-10.
// https://tamagui.dev/bento/elements/datepickers
import type { DatePickerProviderProps } from "@rehookify/datepicker";
import { DatePickerProvider, useDatePickerContext } from "@rehookify/datepicker";
import { ChevronLeft, ChevronRight } from "lucide-react";
import type { ViewProps } from "@tamagui/web";
import type { PopoverProps } from "tamagui";
import {
    Adapt,
    AnimatePresence,
    Button,
    Popover,
    Sheet,
    SizableText,
    View,
    createStyledContext,
    isWeb,
    styled,
    withStaticProperties,
} from "tamagui";

import { Fragment, type ReactNode, useEffect, useRef } from "react";
import { useDateAnimation } from "./useDateAnimation";

type DatePickerProps = PopoverProps & {
    config: DatePickerProviderProps["config"];
};

export type HeaderType = "day" | "month" | "year";

/** rehookify internally return `onClick` and that's incompatible with native */
export function swapOnClick<D>(d: D) {
    //@ts-ignore
    d.onPress = d.onClick;
    return d;
}

export const { Provider: HeaderStyleTypeProvider, useStyledContext: useHeaderType } =
    createStyledContext({
        type: "day",
        setHeader: (_type: HeaderType) => {},
    });

export const HeaderTypeProvider = ({
    config,
    ...props
}: {
    config: DatePickerProviderProps["config"];
    type: HeaderType;
    setHeader: (type: HeaderType) => void;
    children: ReactNode;
}) => {
    return (
        <DatePickerProvider config={config}>
            <HeaderStyleTypeProvider {...props} />
        </DatePickerProvider>
    );
};

const DatePickerImpl = (props: DatePickerProps) => {
    const { children, config, ...rest } = props;
    const popoverRef = useRef<Popover>(null);

    // hide date picker on scroll (web)
    useEffect(() => {
        if (isWeb) {
            const controller = new AbortController();
            // NOTE: For cross-browser compatibility:
            // - We use document.addEventListener('scroll', ...) instead of document.body.addEventListener because Safari does not fire scroll events on body.
            // - We use capture: true because Chrome only fires scroll events on document in the capture phase.
            //   (Chrome works with document.body.addEventListener and capture: false, but that is not reliable in Safari.)
            // This combination ensures the scroll event is caught in both Chrome and Safari.
            document.addEventListener(
                "scroll",
                () => {
                    popoverRef.current?.close();
                },
                {
                    capture: true,
                    signal: controller.signal,
                },
            );

            return () => {
                controller.abort();
            };
        }
    }, []);

    return (
        <DatePickerProvider config={config}>
            <Popover ref={popoverRef} size="$4" allowFlip {...rest}>
                <Adapt when="maxSm">
                    <Sheet modal dismissOnSnapToBottom snapPointsMode="fit">
                        <Sheet.Frame p="$4" width="100%" items="center">
                            <Adapt.Contents />
                        </Sheet.Frame>
                        <Sheet.Overlay
                            transition="lazy"
                            opacity={0.8}
                            enterStyle={{ opacity: 0 }}
                            exitStyle={{ opacity: 0 }}
                        />
                    </Sheet>
                </Adapt>
                {children}
            </Popover>
        </DatePickerProvider>
    );
};

const DatePickerContent = styled(Popover.Content, {
    opacity: 1,
    y: 0,
    transition: "quick",
    variants: {
        unstyled: {
            false: {
                padding: 12,
                borderWidth: 1,
                borderColor: "$borderColor",
                boxShadow: "0px 8px 24px rgba(0, 0, 0, 0.25)",
            },
        },
    } as const,

    defaultVariants: {
        unstyled: process.env.TAMAGUI_HEADLESS === "1",
    },
});

export const DatePicker = withStaticProperties(DatePickerImpl, {
    Trigger: Popover.Trigger,
    Content: withStaticProperties(DatePickerContent, {
        Arrow: styled(Popover.Arrow, {
            borderWidth: 1,
            borderColor: "$borderColor",
        }),
    }),
});

export function MonthPicker({
    onChange = (_e, _date) => {
        "noop";
    },
}: {
    onChange?: (e: MouseEvent, date: Date) => void;
}) {
    const {
        data: { months },
        propGetters: { monthButton },
    } = useDatePickerContext();

    const { prevNextAnimation, prevNextAnimationKey } = useDateAnimation({
        listenTo: "year",
    });

    return (
        <AnimatePresence key={prevNextAnimationKey}>
            <View
                {...prevNextAnimation()}
                flexDirection="row"
                flexWrap="wrap"
                gap="$2"
                transition="100ms"
                grow={0}
                $platform-native={{
                    justify: "space-between",
                    width: "100%",
                }}
                $md={{ width: 285 }}
            >
                {months.map((month) => (
                    <Button
                        theme={month.active ? "blue" : undefined}
                        rounded="$4"
                        shrink={0}
                        flexBasis={90}
                        bg={month.active ? "$background" : "transparent"}
                        key={month.$date.toString()}
                        chromeless
                        p={0}
                        {...swapOnClick(
                            monthButton(month, {
                                onClick: onChange as any,
                            }),
                        )}
                    >
                        <Button.Text color={month.active ? "$color12" : "$color11"}>
                            {month.month}
                        </Button.Text>
                    </Button>
                ))}
            </View>
        </AnimatePresence>
    );
}

export function YearPicker({
    onChange = () => {},
}: {
    onChange?: (e: MouseEvent, date: Date) => void;
}) {
    const {
        data: { years, calendars },
        propGetters: { yearButton },
    } = useDatePickerContext();
    const selectedYear = calendars[0]!.year;

    const { prevNextAnimation, prevNextAnimationKey } = useDateAnimation({
        listenTo: "years",
    });

    return (
        <AnimatePresence key={prevNextAnimationKey}>
            <View
                {...prevNextAnimation()}
                transition={"quick"}
                flexDirection="row"
                flexWrap="wrap"
                gap="$2"
                width="100%"
                $md={{
                    maxW: 280,
                }}
            >
                {years.map((year) => (
                    <Button
                        theme={year.year === Number(selectedYear) ? "blue" : undefined}
                        rounded="$4"
                        flexBasis="30%"
                        grow={1}
                        bg={year.year === Number(selectedYear) ? "$background" : "transparent"}
                        key={year.$date.toString()}
                        chromeless
                        p={0}
                        {...swapOnClick(
                            yearButton(year, {
                                onClick: onChange as any,
                            }),
                        )}
                    >
                        <Button.Text
                            color={year.year === Number(selectedYear) ? "$color12" : "$color11"}
                        >
                            {year.year}
                        </Button.Text>
                    </Button>
                ))}
            </View>
        </AnimatePresence>
    );
}
export function YearRangeSlider() {
    const {
        data: { years },
        propGetters: { previousYearsButton, nextYearsButton },
    } = useDatePickerContext();

    return (
        <View flexDirection="row" width="100%" items="center" justify="space-between">
            <Button circular size="$4" {...swapOnClick(previousYearsButton())}>
                <Button.Icon scaleIcon={1.5}>
                    <ChevronLeft />
                </Button.Icon>
            </Button>
            <View y={2} flexDirection="column" items="center">
                <SizableText size="$5">
                    {`${years[0]!.year} - ${years[years.length - 1]!.year}`}
                </SizableText>
            </View>
            <Button circular size="$4" {...swapOnClick(nextYearsButton())}>
                <Button.Icon scaleIcon={1.5}>
                    <ChevronRight />
                </Button.Icon>
            </Button>
        </View>
    );
}

export function YearSlider() {
    const {
        data: { calendars },
        propGetters: { subtractOffset },
    } = useDatePickerContext();
    const { setHeader } = useHeaderType();
    const { year } = calendars[0]!;
    return (
        <View flexDirection="row" width="100%" height={50} items="center" justify="space-between">
            <Button circular size="$3" {...swapOnClick(subtractOffset({ months: 12 }))}>
                <Button.Icon scaleIcon={1.5}>
                    <ChevronLeft />
                </Button.Icon>
            </Button>
            <SizableText
                onPress={() => setHeader("year")}
                select="text"
                tabIndex={0}
                size="$6"
                cursor="pointer"
                color="$color11"
                hoverStyle={{
                    color: "$color12",
                }}
            >
                {year}
            </SizableText>
            <Button circular size="$3" {...swapOnClick(subtractOffset({ months: -12 }))}>
                <Button.Icon scaleIcon={1.5}>
                    <ChevronRight />
                </Button.Icon>
            </Button>
        </View>
    );
}

export const CalendarHeader = ({
    year,
    month,
    setHeader,
}: {
    year: string;
    month: string;
    setHeader: (header: "year" | "month") => void;
}) => {
    return (
        <View flexDirection="column" height={50} items="center">
            <SizableText
                onPress={() => setHeader("year")}
                tabIndex={0}
                size="$4"
                cursor="pointer"
                color="$color11"
                hoverStyle={{
                    color: "$color12",
                }}
            >
                {year}
            </SizableText>
            <SizableText
                onPress={() => setHeader("month")}
                select="auto"
                tabIndex={0}
                cursor="pointer"
                size="$6"
                color="$color12"
                fontWeight="bold"
                hoverStyle={{
                    color: "$color10",
                }}
            >
                {month}
            </SizableText>
        </View>
    );
};

export const WeekView = ({ weekDays, ...props }: { weekDays: string[]; props?: ViewProps }) => {
    return (
        <View width="100%" flexDirection="row" gap="$1" {...props}>
            {weekDays.map((day) => (
                <Fragment key={day}>
                    <SizableText flex={1} theme="alt1" text="center" width="100%" size="$4">
                        {day}
                    </SizableText>
                </Fragment>
            ))}
        </View>
    );
};
