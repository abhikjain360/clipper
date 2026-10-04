import { requireNativeModule } from "expo-modules-core";
import { Platform } from "react-native";

type ClipboardNative = {
  read: () => { text: string; timestamp: number };
  claim: (id: string, timestamp: number, scope: string) => void;
  install: (id: string, text: string, scope: string) => void;
  clearMissing: (ids: string[], scope: string) => void;
};

export const itemClipboard =
  Platform.OS === "android" ? requireNativeModule<ClipboardNative>("ClipperClipboard") : null;
