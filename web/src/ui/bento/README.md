# Tamagui Bento date picker

Copied from Tamagui's free Bento DatePicker on 2026-09-10:

- Showcase: https://tamagui.dev/bento/elements/datepickers
- Source bundle: https://tamagui.dev/api/bento/code?section=elements&part=datepickers&fileName=DatePicker
- Upstream explicitly lists `DatePicker` in `OSS_COMPONENTS`:
  https://github.com/tamagui/tamagui/blob/main/code/tamagui.dev/app/api/bento/code%2Bapi.ts

Bento distributes this component as copy-and-paste source, not an import from
`tamagui`. The date calculations
and selection state use its upstream dependency, `@rehookify/datepicker`.

Local adaptations:

- Use the existing lucide-react icons and blue theme.
- Use this project's v4 media keys and strict TypeScript indexing.
- Export the picker body; omit the sample and unused input implementation.
- Supply the application's date-range button and controlled date in
  `CalendarDatePicker.tsx`.
- Add accessible day/arrow labels and compact day buttons.
- Omit the popover's entry/exit opacity animation: the supplied variant remained
  invisible with this application's animation configuration. The mobile Sheet
  and inner calendar transitions retain the upstream behavior.

Retain these notes when updating from the upstream source.
