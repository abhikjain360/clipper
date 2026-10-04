import { requireNativeModule } from "expo-modules-core";
import { Platform } from "react-native";

type ClipboardNative = {
  read: () => { text: string; timestamp: number; token: string };
  claim: (id: string, timestamp: number, scope: string, token: string) => void;
  install: (id: string, text: string, scope: string) => void;
  clearDeleted: (ids: string[], scope: string) => void;
  reset: () => void;
};

export const itemClipboard =
  Platform.OS === "android" ? requireNativeModule<ClipboardNative>("ClipperClipboard") : null;
