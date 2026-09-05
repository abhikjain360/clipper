import { useState, type ReactNode } from "react";
import { Button, Input, Popover, XStack, YStack } from "tamagui";
import { ChevronLeft, ChevronRight } from "lucide-react";
import { calendarWindow, movePeriod, periodStart } from "./calendar-view";

export function CalendarDatePicker({
    value,
    onChange,
    children,
}: {
    value: Date;
    onChange: (date: Date) => void;
    children: ReactNode;
}) {
    const [open, setOpen] = useState(false);
    const [month, setMonth] = useState(() => periodStart(value, "month"));
    const [year, setYear] = useState(String(value.getFullYear()));
    const browse = (date: Date) => {
        setMonth(date);
        setYear(String(date.getFullYear()));
    };
    const choose = (date: Date) => {
        onChange(date);
        setOpen(false);
    };
    const { start, end } = calendarWindow(month, "month");
    const days: Date[] = [];
    for (
        let day = new Date(start);
        day < end;
        day = new Date(day.getFullYear(), day.getMonth(), day.getDate() + 1)
    )
        days.push(new Date(day));
    return (
        <Popover
            open={open}
            onOpenChange={(next) => {
                if (next) browse(periodStart(value, "month"));
                setOpen(next);
            }}
            placement="bottom"
            allowFlip
            stayInFrame
        >
            <Popover.Trigger asChild>
                <Button size="$2" aria-label="Choose calendar date" aria-live="polite">
                    {children}
                </Button>
            </Popover.Trigger>
            <Popover.Content
                p="$3"
                bg="#222a33"
                borderWidth={1}
                borderColor="#526171"
                width={300}
                maxW="calc(100vw - 24px)"
                elevate
            >
                <YStack gap="$2">
                    <XStack items="center" justify="space-between" gap="$2">
                        <Button
                            size="$2"
                            aria-label="Previous picker month"
                            icon={<ChevronLeft size={16} />}
                            onPress={() => browse(movePeriod(month, "month", -1))}
                        />
                        <span aria-live="polite">
                            {month.toLocaleDateString(undefined, { month: "long" })}
                        </span>
                        <Input
                            aria-label="Picker year"
                            width={78}
                            size="$2"
                            inputMode="numeric"
                            value={year}
                            onChangeText={(text) => {
                                setYear(text);
                                if (/^\d{4}$/.test(text) && Number(text) >= 1000) {
                                    const next = new Date(month);
                                    next.setFullYear(Number(text));
                                    setMonth(next);
                                }
                            }}
                            onBlur={() => setYear(String(month.getFullYear()))}
                        />
                        <Button
                            size="$2"
                            aria-label="Next picker month"
                            icon={<ChevronRight size={16} />}
                            onPress={() => browse(movePeriod(month, "month", 1))}
                        />
                    </XStack>
                    <div className="date-picker-grid">
                        {["Mo", "Tu", "We", "Th", "Fr", "Sa", "Su"].map((day) => (
                            <span className="date-picker-weekday" key={day}>
                                {day}
                            </span>
                        ))}
                        {days.map((day) => (
                            <button
                                key={day.toISOString()}
                                className={`date-picker-day${day.getMonth() !== month.getMonth() ? " outside-month" : ""}`}
                                aria-label={day.toLocaleDateString(undefined, {
                                    weekday: "long",
                                    year: "numeric",
                                    month: "long",
                                    day: "numeric",
                                })}
                                aria-pressed={day.toDateString() === value.toDateString()}
                                aria-current={
                                    day.toDateString() === new Date().toDateString()
                                        ? "date"
                                        : undefined
                                }
                                onClick={() => choose(day)}
                            >
                                {day.getDate()}
                            </button>
                        ))}
                    </div>
                    <Button size="$2" onPress={() => choose(periodStart(new Date(), "day"))}>
                        Today
                    </Button>
                </YStack>
            </Popover.Content>
        </Popover>
    );
}
