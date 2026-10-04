import { requireNativeViewManager } from "expo-modules-core";
import { Platform, type NativeSyntheticEvent, type ViewProps } from "react-native";
import type { EditorEvent, EditorState } from "../../src/collabText";

type CollabInputProps = ViewProps & {
  state: EditorState;
  editable: boolean;
  fontFamily: string;
  colors: { text: string; background: string; selection: string; cursor: string };
  onEdit: (event: NativeSyntheticEvent<EditorEvent>) => void;
};

export const CollabInput =
  Platform.OS === "android" ? requireNativeViewManager<CollabInputProps>("ClipperEditor") : null;
