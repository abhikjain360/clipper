import { LANGUAGE_OPTIONS } from "./languages";

// A compact native-select language picker for the editor toolbar. Native (vs a
// Tamagui Select) keeps it dependency-free, accessible, and identical on the
// chrome-less public share page. Styled to match the dark editor surface.
export function LanguageSelect({
    value,
    onChange,
    disabled,
}: {
    value: string;
    onChange: (id: string) => void;
    disabled?: boolean;
}) {
    return (
        <select
            className="editor-select"
            value={value}
            disabled={disabled}
            aria-label="Editor language"
            onChange={(event) => onChange(event.currentTarget.value)}
            style={{
                borderRadius: 6,
                padding: "4px 8px",
                fontSize: 12,
                fontFamily: "inherit",
                cursor: disabled ? "default" : "pointer",
            }}
        >
            {LANGUAGE_OPTIONS.map((option) => (
                <option key={option.id} value={option.id}>
                    {option.label}
                </option>
            ))}
        </select>
    );
}
