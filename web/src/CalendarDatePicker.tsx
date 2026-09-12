import { useState, type ReactNode } from "react";
import { Button } from "tamagui";
import type { DatePickerProviderProps } from "@rehookify/datepicker";
import { DatePickerBody } from "./ui/bento/DatePicker";
import { DatePicker } from "./ui/bento/common/dateParts";

export function CalendarDatePicker({
    value,
    onChange,
    children,
    id,
    label = "Choose calendar date",
}: {
    id?: string;
    label?: string;
    value: Date;
    onChange: (date: Date) => void;
    children: ReactNode;
}) {
    const [open, setOpen] = useState(false);
    const config: DatePickerProviderProps["config"] = {
        selectedDates: [value],
        dates: { mode: "single" },
        calendar: { startDay: 1 },
        onDatesChange: (dates) => {
            if (dates[0]) {
                onChange(dates[0]);
                setOpen(false);
            }
        },
    };
    return (
        <DatePicker open={open} onOpenChange={setOpen} config={config}>
            <DatePicker.Trigger asChild>
                <Button size="$2" id={id} aria-label={label} aria-live="polite">
                    {children}
                </Button>
            </DatePicker.Trigger>
            {open && (
                <DatePicker.Content width={340} maxW="calc(100vw - 24px)">
                    <DatePicker.Content.Arrow />
                    {open && (
                        <>
                            <DatePickerBody config={config} />
                            <Button
                                size="$2"
                                onPress={() => {
                                    const today = new Date();
                                    today.setHours(0, 0, 0, 0);
                                    onChange(today);
                                    setOpen(false);
                                }}
                            >
                                Today
                            </Button>
                        </>
                    )}
                </DatePicker.Content>
            )}
        </DatePicker>
    );
}
