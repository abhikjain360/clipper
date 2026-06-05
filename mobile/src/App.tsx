import {
  Clipboard,
  Copy,
  Download,
  FileUp,
  Files,
  Folder,
  LogOut,
  RefreshCw,
  Trash2,
} from "lucide-react-native";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { SafeAreaView, StatusBar } from "react-native";
import { TamaguiProvider } from "tamagui";
import {
  Button,
  Card,
  H1,
  H2,
  Input,
  Label,
  Paragraph,
  ScrollView,
  Spinner,
  Tabs,
  Text,
  XStack,
  YStack,
} from "tamagui";
import type { AppState, ClipboardItem, FileItem } from "@clipper/shared";
import {
  backend,
  formatBackendError,
  pickUploadFile,
  readClipboardText,
  shareDownloadedFile,
  writeClipboardText,
} from "./backend";
import tamaguiConfig from "./tamagui.config";

type TabName = "clipboard" | "files";

export default function App() {
  return (
    <TamaguiProvider config={tamaguiConfig} defaultTheme="dark">
      <StatusBar barStyle="light-content" />
      <SafeAreaView style={{ flex: 1, backgroundColor: "#101214" }}>
        <ClipperApp />
      </SafeAreaView>
    </TamaguiProvider>
  );
}

function ClipperApp() {
  const [state, setState] = useState<AppState | null>(null);
  const [startupError, setStartupError] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;

    async function run() {
      try {
        await backend.connect();
        let seenVersion = await backend.stateVersion();
        if (cancelled) return;
        setState(await backend.getState());

        while (!cancelled) {
          seenVersion = await backend.waitForStateChange(seenVersion);
          if (!cancelled) setState(await backend.getState());
        }
      } catch (caught) {
        if (!cancelled) setStartupError(formatBackendError(caught));
      }
    }

    void run();

    return () => {
      cancelled = true;
    };
  }, []);

  if (startupError) {
    return <CenteredStatus title="Cannot start Clipper" message={startupError} />;
  }

  if (!state) return <CenteredStatus title="Starting Clipper" loading />;

  if (!state.session) {
    return <LoginScreen initialUsername={state.saved_profile?.username ?? ""} onState={setState} />;
  }

  return <HomeScreen state={state} onState={setState} />;
}

function LoginScreen({
  initialUsername,
  onState,
}: {
  initialUsername: string;
  onState: (state: AppState) => void;
}) {
  const [mode, setMode] = useState<"login" | "register">("login");
  const [serverUrl, setServerUrl] = useState("");
  const [username, setUsername] = useState(initialUsername);
  const [passphrase, setPassphrase] = useState("");
  const [accessKey, setAccessKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const busyRef = useRef(false);

  useEffect(() => {
    void Promise.resolve(backend.defaultServerUrl())
      .then(setServerUrl)
      .catch(() => setServerUrl("http://127.0.0.1:8787"));
  }, []);

  async function authenticate() {
    if (busyRef.current) return;

    busyRef.current = true;
    setBusy(true);
    setError(null);

    try {
      if (mode === "login") {
        await backend.login(passphrase, username, "", serverUrl);
      } else {
        await backend.register(accessKey, username, passphrase, "", serverUrl);
      }
      onState(await backend.getState());
    } catch (caught) {
      setError(formatBackendError(caught));
    } finally {
      busyRef.current = false;
      setBusy(false);
    }
  }

  return (
    <YStack flex={1} items="center" justify="center" p="$4" bg="#101214">
      <Card width="100%" maxW={460} p="$5" bg="#171a1d" borderColor="#252b31" borderWidth={1}>
        <YStack gap="$4">
          <YStack gap="$2">
            <H1 size="$9">Clipper</H1>
            <Paragraph color="#9aa4ad">Encrypted clipboard and file sync</Paragraph>
          </YStack>

          <XStack gap="$2">
            <Button
              flex={1}
              theme={mode === "login" ? "blue" : undefined}
              onPress={() => setMode("login")}
            >
              Login
            </Button>
            <Button
              flex={1}
              theme={mode === "register" ? "blue" : undefined}
              onPress={() => setMode("register")}
            >
              Register
            </Button>
          </XStack>

          <Field label="Server URL">
            <Input
              value={serverUrl}
              autoCapitalize="none"
              autoCorrect={false}
              onChangeText={setServerUrl}
            />
          </Field>
          <Field label="Username">
            <Input
              value={username}
              autoCapitalize="none"
              autoCorrect={false}
              onChangeText={setUsername}
            />
          </Field>
          {mode === "register" && (
            <Field label="Access key">
              <Input
                value={accessKey}
                autoCapitalize="none"
                autoCorrect={false}
                onChangeText={setAccessKey}
              />
            </Field>
          )}
          <Field label="Passphrase">
            <Input value={passphrase} secureTextEntry onChangeText={setPassphrase} />
          </Field>

          {error && <Paragraph color="#ff7b7b">{error}</Paragraph>}

          <Button
            theme="blue"
            disabled={busy}
            icon={busy ? <Spinner /> : undefined}
            onPress={() => void authenticate()}
          >
            {mode === "login" ? "Login" : "Register"}
          </Button>
        </YStack>
      </Card>
    </YStack>
  );
}

function HomeScreen({ state, onState }: { state: AppState; onState: (state: AppState) => void }) {
  const [tab, setTab] = useState<TabName>("clipboard");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function refresh() {
    setBusy(true);
    setError(null);
    try {
      await backend.refresh();
      onState(await backend.getState());
    } catch (caught) {
      setError(formatBackendError(caught));
    } finally {
      setBusy(false);
    }
  }

  async function logout() {
    setError(null);
    try {
      await backend.logout();
      onState(await backend.getState());
    } catch (caught) {
      setError(formatBackendError(caught));
    }
  }

  return (
    <YStack flex={1} bg="#101214">
      <XStack
        items="center"
        justify="space-between"
        gap="$3"
        px="$4"
        py="$3"
        bg="#171a1d"
        borderBottomColor="#252b31"
        borderBottomWidth={1}
      >
        <YStack>
          <H2 size="$7">Clipper</H2>
          <Paragraph size="$2" color="#9aa4ad">
            {state.connection_status}
          </Paragraph>
        </YStack>
        <XStack items="center" gap="$2">
          <Button
            size="$3"
            icon={busy ? <Spinner /> : <RefreshCw size={16} />}
            onPress={refresh}
            disabled={busy}
          />
          <Button size="$3" icon={<LogOut size={16} />} onPress={() => void logout()} />
        </XStack>
      </XStack>

      <YStack p="$4" gap="$3" flex={1}>
        <Tabs
          value={tab}
          onValueChange={(value) => setTab(value as TabName)}
          flex={1}
          orientation="horizontal"
        >
          <Tabs.List>
            <Tabs.Tab value="clipboard" flex={1}>
              <XStack items="center" gap="$2">
                <Clipboard size={16} />
                <Text>Clipboard</Text>
              </XStack>
            </Tabs.Tab>
            <Tabs.Tab value="files" flex={1}>
              <XStack items="center" gap="$2">
                <Folder size={16} />
                <Text>Files</Text>
              </XStack>
            </Tabs.Tab>
          </Tabs.List>

          {error && <Paragraph color="#ff7b7b">{error}</Paragraph>}

          <Tabs.Content value="clipboard" flex={1}>
            <ClipboardPanel items={state.clipboard_items} onState={onState} onError={setError} />
          </Tabs.Content>
          <Tabs.Content value="files" flex={1}>
            <FilesPanel files={state.files} onState={onState} onError={setError} />
          </Tabs.Content>
        </Tabs>
      </YStack>
    </YStack>
  );
}

function ClipboardPanel({
  items,
  onState,
  onError,
}: {
  items: ClipboardItem[];
  onState: (state: AppState) => void;
  onError: (error: string | null) => void;
}) {
  const [busy, setBusy] = useState(false);

  async function addClipboardText() {
    setBusy(true);
    onError(null);
    try {
      const text = await readClipboardText();
      if (!text) {
        onError("Clipboard is empty or unavailable");
        return;
      }
      await backend.sendClipboardText(text);
      onState(await backend.getState());
    } catch (caught) {
      onError(formatBackendError(caught));
    } finally {
      setBusy(false);
    }
  }

  async function copyItem(item: ClipboardItem) {
    onError(null);
    try {
      const payload = await backend.clipboardPayload(item.id);
      if (payload.text === null) {
        onError(`Cannot copy ${payload.mimeType} to the text clipboard`);
        return;
      }
      await writeClipboardText(payload.text);
    } catch (caught) {
      onError(formatBackendError(caught));
    }
  }

  return (
    <YStack gap="$3" flex={1} pt="$3">
      <XStack justify="space-between" items="center" gap="$2">
        <H2 size="$6">Clipboard</H2>
        <Button
          icon={busy ? <Spinner /> : <Copy size={16} />}
          onPress={() => void addClipboardText()}
          disabled={busy}
        >
          Add Current
        </Button>
      </XStack>

      {items.length === 0 ? (
        <EmptyState icon={<Clipboard size={28} />} title="No clipboard items yet" />
      ) : (
        <ScrollView>
          <YStack gap="$2" pb="$4">
            {items.map((item) => (
              <ListCard key={item.id}>
                <XStack items="center" justify="space-between" gap="$3">
                  <YStack flex={1} gap="$1">
                    <Text numberOfLines={3}>{item.text}</Text>
                    <Paragraph size="$2" color="#9aa4ad">
                      {item.mime_type} - {formatRelativeTime(item.created_at)}
                    </Paragraph>
                  </YStack>
                  <Button size="$3" icon={<Copy size={16} />} onPress={() => void copyItem(item)} />
                </XStack>
              </ListCard>
            ))}
          </YStack>
        </ScrollView>
      )}
    </YStack>
  );
}

function FilesPanel({
  files,
  onState,
  onError,
}: {
  files: FileItem[];
  onState: (state: AppState) => void;
  onError: (error: string | null) => void;
}) {
  const [busy, setBusy] = useState(false);

  async function uploadFile() {
    setBusy(true);
    onError(null);
    try {
      const file = await pickUploadFile();
      if (!file) return;
      await backend.uploadFileBytes(file.filename, file.mimeType, file.bytes);
      onState(await backend.getState());
    } catch (caught) {
      onError(formatBackendError(caught));
    } finally {
      setBusy(false);
    }
  }

  async function downloadFile(file: FileItem) {
    onError(null);
    try {
      const bytes = await backend.downloadFileBytes(file.id);
      await shareDownloadedFile(file.filename, file.mime_type, bytes);
    } catch (caught) {
      onError(formatBackendError(caught));
    }
  }

  async function deleteFile(file: FileItem) {
    onError(null);
    try {
      await backend.deleteFile(file.id);
      onState(await backend.getState());
    } catch (caught) {
      onError(formatBackendError(caught));
    }
  }

  return (
    <YStack gap="$3" flex={1} pt="$3">
      <XStack justify="space-between" items="center" gap="$2">
        <H2 size="$6">Files</H2>
        <Button
          icon={busy ? <Spinner /> : <FileUp size={16} />}
          onPress={() => void uploadFile()}
          disabled={busy}
        >
          Upload File
        </Button>
      </XStack>

      {files.length === 0 ? (
        <EmptyState icon={<Folder size={28} />} title="No files yet" />
      ) : (
        <ScrollView>
          <YStack gap="$2" pb="$4">
            {files.map((file) => (
              <ListCard key={file.id}>
                <XStack items="center" justify="space-between" gap="$3">
                  <XStack items="center" gap="$3" flex={1}>
                    <Files size={22} color="#6fb4ff" />
                    <YStack flex={1} gap="$1">
                      <Text numberOfLines={1}>{file.filename}</Text>
                      <Paragraph size="$2" color="#9aa4ad">
                        {formatByteSize(file.blob_size)} - {formatRelativeTime(file.created_at)}
                      </Paragraph>
                    </YStack>
                  </XStack>
                  <XStack gap="$1">
                    <Button
                      size="$3"
                      icon={<Download size={16} />}
                      onPress={() => void downloadFile(file)}
                    />
                    <Button
                      size="$3"
                      icon={<Trash2 size={16} color="#ff6b6b" />}
                      onPress={() => void deleteFile(file)}
                    />
                  </XStack>
                </XStack>
              </ListCard>
            ))}
          </YStack>
        </ScrollView>
      )}
    </YStack>
  );
}

function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <YStack gap="$2">
      <Label color="#cdd5dc">{label}</Label>
      {children}
    </YStack>
  );
}

function ListCard({ children }: { children: ReactNode }) {
  return (
    <Card p="$3" bg="#171a1d" borderColor="#252b31" borderWidth={1}>
      {children}
    </Card>
  );
}

function EmptyState({ icon, title }: { icon: ReactNode; title: string }) {
  return (
    <YStack flex={1} items="center" justify="center" gap="$3" p="$4">
      {icon}
      <Paragraph color="#9aa4ad">{title}</Paragraph>
    </YStack>
  );
}

function CenteredStatus({
  title,
  message,
  loading,
}: {
  title: string;
  message?: string;
  loading?: boolean;
}) {
  return (
    <YStack flex={1} items="center" justify="center" gap="$3" p="$4" bg="#101214">
      {loading && <Spinner size="large" />}
      <H2>{title}</H2>
      {message && <Paragraph color="#9aa4ad">{message}</Paragraph>}
    </YStack>
  );
}

function formatByteSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  const units = ["KB", "MB", "GB"];
  let value = bytes / 1024;
  let unit = units[0] ?? "KB";
  for (let i = 1; value >= 1024 && i < units.length; i += 1) {
    value /= 1024;
    unit = units[i] ?? unit;
  }
  return `${value.toFixed(value >= 10 ? 0 : 1)} ${unit}`;
}

function formatRelativeTime(value: string): string {
  const time = Date.parse(value);
  if (Number.isNaN(time)) return value;

  const seconds = Math.max(0, Math.floor((Date.now() - time) / 1000));
  if (seconds < 60) return `${seconds}s ago`;
  const minutes = Math.floor(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.floor(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.floor(hours / 24);
  return `${days}d ago`;
}
