// Adapted from the free Tamagui Bento DatePicker, retrieved 2026-09-10.
// https://tamagui.dev/bento/elements/datepickers
import {
    useDatePickerContext,
    type DatePickerProviderProps,
    type DPDay,
} from "@rehookify/datepicker";

import { ChevronLeft, ChevronRight } from "lucide-react";
import { useMemo, useState } from "react";
import { AnimatePresence, Button, H3, View } from "tamagui";

import {
    CalendarHeader,
    HeaderTypeProvider,
    MonthPicker,
    swapOnClick,
    useHeaderType,
    WeekView,
    YearPicker,
    YearRangeSlider,
    type HeaderType,
} from "./common/dateParts";
import { useDateAnimation } from "./common/useDateAnimation";

function DateHeader() {
    const {
        data: { calendars },
        propGetters: { subtractOffset },
    } = useDatePickerContext();
    const { type: header, setHeader } = useHeaderType();
    const { year, month } = calendars[0]!;

    if (header === "year") {
        return <YearRangeSlider />;
    }

    if (header === "month") {
        return (
            <H3 size="$7" self="center">
                Select a month
            </H3>
        );
    }
    return (
        <View flexDirection="row" width="100%" items="center" justify="space-between">
            <Button
                aria-label="Previous picker month"
                circular
                size="$4"
                {...swapOnClick(subtractOffset({ months: 1 }))}
            >
                <Button.Icon scaleIcon={1.5}>
                    <ChevronLeft />
                </Button.Icon>
            </Button>

            <CalendarHeader year={year} month={month} setHeader={setHeader} />

            <Button
                aria-label="Next picker month"
                circular
                size="$4"
                {...swapOnClick(subtractOffset({ months: -1 }))}
            >
                <Button.Icon scaleIcon={1.5}>
                    <ChevronRight />
                </Button.Icon>
            </Button>
        </View>
    );
}

function DayPicker() {
    const {
        data: { calendars, weekDays },
        propGetters: { dayButton },
    } = useDatePickerContext();

    const { days } = calendars[0]!;

    const { prevNextAnimation, prevNextAnimationKey } = useDateAnimation({
        listenTo: "month",
    });

    // divide days array into sub arrays that each has 7 days, for better stylings
    const subDays = useMemo(
        () =>
            days.reduce((acc, day, i) => {
                if (i % 7 === 0) {
                    acc.push([]);
                }
                acc[acc.length - 1]!.push(day);
                return acc;
            }, [] as DPDay[][]),
        [days],
    );

    return (
        <AnimatePresence key={prevNextAnimationKey}>
            <View width="100%" gap="$4" transition="medium" {...prevNextAnimation()}>
                <WeekView weekDays={weekDays} />

                <View flexDirection="column" gap="$2" items="center" justify="center" width="100%">
                    {subDays.map((week) => {
                        return (
                            <View
                                justify="space-between"
                                items="center"
                                flexDirection="row"
                                key={week[0]!.$date.toString()}
                                gap="$1"
                                flex={1}
                                flexBasis="auto"
                                width="100%"
                            >
                                {week.map((d) => (
                                    <Button
                                        key={d.$date.toString()}
                                        size="$3"
                                        aria-label={d.$date.toLocaleDateString(undefined, {
                                            year: "numeric",
                                            month: "long",
                                            day: "numeric",
                                        })}
                                        chromeless
                                        circular
                                        p={0}
                                        {...swapOnClick(dayButton(d))}
                                        bg={d.selected ? "$background" : "transparent"}
                                        theme={d.selected ? "blue" : undefined}
                                        disabled={!d.inCurrentMonth}
                                    >
                                        <Button.Text
                                            fontWeight="500"
                                            fontSize="$4"
                                            color={
                                                d.selected
                                                    ? "$color12"
                                                    : d.inCurrentMonth
                                                      ? "$color11"
                                                      : "$color6"
                                            }
                                        >
                                            {d.day}
                                        </Button.Text>
                                    </Button>
                                ))}
                            </View>
                        );
                    })}
                </View>
            </View>
        </AnimatePresence>
    );
}

export function DatePickerBody({ config }: { config: DatePickerProviderProps["config"] }) {
    const [header, setHeader] = useState<HeaderType>("day");

    return (
        <HeaderTypeProvider config={config} type={header} setHeader={setHeader}>
            <View
                flexDirection="column"
                items="center"
                gap="$4"
                width="100%"
                p="$4"
                $md={{ p: "$2" }}
            >
                <DateHeader />
                {header === "month" && <MonthPicker onChange={() => setHeader("day")} />}
                {header === "year" && <YearPicker onChange={() => setHeader("day")} />}
                {header === "day" && <DayPicker />}
            </View>
        </HeaderTypeProvider>
    );
}
