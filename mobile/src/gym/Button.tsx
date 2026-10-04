import { useRef, type ComponentProps } from "react";
import { createPressGuard } from "@clipper/shared";
import { Spinner } from "tamagui";
import { Button as AppButton } from "../tamagui.config";

export function Button({
  busy,
  disabled,
  icon,
  onPress,
  ...props
}: ComponentProps<typeof AppButton> & { busy?: boolean }) {
  const accept = useRef(createPressGuard());
  return (
    <AppButton
      {...props}
      disabled={disabled || busy}
      accessibilityState={{ disabled: disabled || busy, busy }}
      icon={busy ? <Spinner /> : icon}
      pressStyle={{ opacity: 0.65 }}
      onPress={(event) => {
        if (busy || disabled || (busy !== undefined && !accept.current(Date.now()))) return;
        onPress?.(event);
      }}
    />
  );
}
