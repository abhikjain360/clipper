import {
    ArrowLeft,
    Clipboard,
    Copy,
    Download,
    Eye,
    FileCode,
    FilePlus,
    FileText,
    FileUp,
    Files,
    Folder,
    LogOut,
    RefreshCw,
    Smartphone,
    Trash2,
    X,
} from "lucide-react";
import {
    Suspense,
    lazy,
    useCallback,
    useEffect,
    useRef,
    useState,
    type FormEvent,
    type ReactNode,
} from "react";
import { Route, Switch, useLocation } from "wouter";
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
    Text,
    XStack,
    YStack,
} from "tamagui";
import {
    clearSessionCredentials,
    clipperBackend,
    defaultServerUrl,
    formatBackendError,
    isTauriRuntime,
    readClipboardText,
    resumeSession,
    saveSessionCredentials,
    writeClipboardText,
} from "./backend";
import type { AppState, ClipboardItem, CollabItem, DeviceInfo, FileItem } from "@clipper/shared";
import { ErrorBoundary } from "./ErrorBoundary";

// Lazy-loaded so the heavy CodeMirror dependency (editor core, vim mode, and the
// per-language packs) splits into its own chunk and stays off the initial load
// path — it is only fetched when a file/doc is actually opened.
const CodeEditor = lazy(() =>
    import("./CodeEditor").then((module) => ({ default: module.CodeEditor })),
);

const codeEditorFallback = (
    <div
        style={{
            flex: 1,
            minHeight: 0,
            display: "grid",
            placeItems: "center",
            color: "#5b6571",
        }}
    >
        Loading editor…
    </div>
);

// Shown if the editor subtree throws (e.g. a chunk fails to load/mount). Keeps
// the failure local to the editor slot instead of blanking the whole app.
const renderEditorError = (error: Error): ReactNode => (
    <div
        style={{
            flex: 1,
            minHeight: 0,
            display: "grid",
            placeItems: "center",
            padding: 16,
            textAlign: "center",
        }}
    >
        <div>
            <div style={{ color: "#ff8f8f", fontWeight: 600, marginBottom: 6 }}>
                Editor failed to load
            </div>
            <div
                style={{
                    color: "#9aa4ad",
                    fontSize: 12,
                    fontFamily: "ui-monospace, SFMono-Regular, Menlo, monospace",
                    maxWidth: 520,
                }}
            >
                {error.message}
            </div>
        </div>
    </div>
);

const VIM_MODE_STORAGE_KEY = "clipper_vim_mode";

export default function App() {
    const [state, setState] = useState<AppState | null>(null);
    const [startupError, setStartupError] = useState<string | null>(null);

    useEffect(() => {
        let cancelled = false;

        async function run() {
            try {
                const backend = await clipperBackend();
                await backend.connect();
                // Browser only: replay a stored login so a reload skips the
                // login screen. No-op under Tauri or when nothing is stored.
                await resumeSession();
                let seenVersion = await backend.stateVersion();
                if (cancelled) return;
                setState(await backend.getState());

                /* eslint-disable no-await-in-loop, no-unmodified-loop-condition */
                while (!cancelled) {
                    seenVersion = await backend.waitForStateChange(seenVersion);
                    if (!cancelled) setState(await backend.getState());
                }
                /* eslint-enable no-await-in-loop, no-unmodified-loop-condition */
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
        return (
            <CenteredStatus
                title="Cannot start Clipper"
                message={`${startupError}. Use nix run .#web-serve for the browser client or nix run .#tauri-dev for the native shell.`}
            />
        );
    }

    if (!state) {
        return <CenteredStatus title="Starting Clipper" loading />;
    }

    if (!state.session) {
        return (
            <LoginScreen initialUsername={state.saved_profile?.username ?? ""} onState={setState} />
        );
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
        // Hosted builds bake the production API in via VITE_SERVER_URL; dev
        // builds fall through to the wasm default (localhost).
        const envUrl = import.meta.env.VITE_SERVER_URL as string | undefined;
        if (envUrl) {
            setServerUrl(envUrl);
            return;
        }
        void defaultServerUrl()
            .then(setServerUrl)
            .catch(() => setServerUrl("http://127.0.0.1:8787"));
    }, []);

    async function authenticate() {
        if (busyRef.current) return;

        busyRef.current = true;
        setBusy(true);
        setError(null);

        try {
            const backend = await clipperBackend();
            if (mode === "login") {
                await backend.login(passphrase, username, "", serverUrl);
            } else {
                await backend.register(accessKey, username, passphrase, "", serverUrl);
            }
            // Persist for reload resume (browser only; sessionStorage).
            saveSessionCredentials({ passphrase, username, deviceName: "", serverUrl });
            onState(await backend.getState());
        } catch (caught) {
            setError(formatBackendError(caught));
        } finally {
            busyRef.current = false;
            setBusy(false);
        }
    }

    function submit(event: FormEvent<HTMLFormElement>) {
        event.preventDefault();
        void authenticate();
    }

    return (
        <YStack minH="100vh" items="center" justify="center" p="$4">
            <Card
                width="100%"
                maxW={460}
                p="$5"
                bg="#171a1d"
                style={{ borderColor: "#252b31", borderWidth: 1 }}
            >
                <form onSubmit={submit}>
                    <YStack gap="$4">
                        <YStack gap="$2">
                            <H1 size="$9">Clipper</H1>
                            <Paragraph color="#9aa4ad">Encrypted clipboard and file sync</Paragraph>
                        </YStack>

                        <XStack gap="$2">
                            <Button
                                type="button"
                                flex={1}
                                theme={mode === "login" ? "blue" : undefined}
                                onPress={() => setMode("login")}
                            >
                                Login
                            </Button>
                            <Button
                                type="button"
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
                                autoCorrect="off"
                                onChangeText={setServerUrl}
                            />
                        </Field>
                        <Field label="Username">
                            <Input
                                value={username}
                                autoCapitalize="none"
                                autoCorrect="off"
                                onChangeText={setUsername}
                            />
                        </Field>
                        {mode === "register" && (
                            <Field label="Access key">
                                <Input
                                    value={accessKey}
                                    autoCapitalize="none"
                                    autoCorrect="off"
                                    onChangeText={setAccessKey}
                                />
                            </Field>
                        )}
                        <Field label="Passphrase">
                            <Input
                                value={passphrase}
                                secureTextEntry
                                type="password"
                                onChangeText={setPassphrase}
                            />
                        </Field>

                        {error && <Paragraph color="#ff7b7b">{error}</Paragraph>}

                        <Button
                            type="submit"
                            theme="blue"
                            disabled={busy}
                            icon={busy ? <Spinner /> : undefined}
                        >
                            {mode === "login" ? "Login" : "Register"}
                        </Button>
                    </YStack>
                </form>
            </Card>
        </YStack>
    );
}

function HomeScreen({ state, onState }: { state: AppState; onState: (state: AppState) => void }) {
    const [location, setLocation] = useLocation();
    const [busy, setBusy] = useState(false);
    const [error, setError] = useState<string | null>(null);

    async function refresh() {
        setBusy(true);
        setError(null);
        try {
            const backend = await clipperBackend();
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
        clearSessionCredentials();
        try {
            const backend = await clipperBackend();
            await backend.logout();
            onState(await backend.getState());
        } catch (caught) {
            setError(formatBackendError(caught));
        }
    }

    return (
        <YStack minH="100vh" bg="#101214">
            <XStack
                items="center"
                justify="space-between"
                gap="$3"
                px="$4"
                py="$3"
                bg="#171a1d"
                flexWrap="wrap"
                style={{ borderBottomColor: "#252b31", borderBottomWidth: 1 }}
            >
                <XStack items="center" gap="$3">
                    <H2 size="$7">Clipper</H2>
                    <ConnectionBadge status={state.connection_status} />
                </XStack>
                <XStack items="center" gap="$2">
                    <Button
                        size="$3"
                        icon={busy ? <Spinner /> : <RefreshCw size={16} />}
                        onPress={refresh}
                        disabled={busy}
                    >
                        Refresh
                    </Button>
                    <Button size="$3" icon={<LogOut size={16} />} onPress={logout}>
                        Logout
                    </Button>
                </XStack>
            </XStack>

            <YStack width="100%" maxW={1100} self="center" p="$4" gap="$3" flex={1}>
                <XStack gap="$2" flexWrap="wrap">
                    <Button
                        theme={location === "/" ? "blue" : undefined}
                        icon={<Clipboard size={16} />}
                        onPress={() => setLocation("/")}
                    >
                        Clipboard
                    </Button>
                    <Button
                        theme={location === "/files" ? "blue" : undefined}
                        icon={<Folder size={16} />}
                        onPress={() => setLocation("/files")}
                    >
                        Files
                    </Button>
                    <Button
                        theme={location.startsWith("/collab") ? "blue" : undefined}
                        icon={<FileText size={16} />}
                        onPress={() => setLocation("/collab")}
                    >
                        Collab Docs
                    </Button>
                    <Button
                        theme={location === "/devices" ? "blue" : undefined}
                        icon={<Smartphone size={16} />}
                        onPress={() => setLocation("/devices")}
                    >
                        Devices
                    </Button>
                </XStack>

                {error && <Paragraph color="#ff7b7b">{error}</Paragraph>}

                <Switch>
                    <Route path="/files">
                        <FilesPanel files={state.files} onState={onState} onError={setError} />
                    </Route>
                    <Route path="/collab/:id">
                        {(params) => <CollabDocView id={params.id} onError={setError} />}
                    </Route>
                    <Route path="/collab">
                        <CollabPanel
                            collabDocs={state.collab_docs}
                            onState={onState}
                            onError={setError}
                        />
                    </Route>
                    <Route path="/devices">
                        <DevicesPanel onError={setError} />
                    </Route>
                    <Route>
                        <ClipboardPanel
                            items={state.clipboard_items}
                            onState={onState}
                            onError={setError}
                        />
                    </Route>
                </Switch>
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
    const nativeRuntime = isTauriRuntime();

    async function addClipboardText() {
        setBusy(true);
        onError(null);
        try {
            const backend = await clipperBackend();
            if (nativeRuntime && backend.sendCurrentClipboardText) {
                const itemId = await backend.sendCurrentClipboardText();
                if (!itemId) {
                    onError("Clipboard is empty or unavailable");
                    return;
                }
                onState(await backend.getState());
                return;
            }

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
            const backend = await clipperBackend();
            if (nativeRuntime && backend.writeClipboardItemText) {
                await backend.writeClipboardItemText(item.id);
                return;
            }

            const payload = await backend.clipboardPayload(item.id);
            if (payload.text == null) {
                // Non-text MIME (e.g. an image synced from another device):
                // decoding bytes as UTF-8 would write mojibake to the clipboard.
                // Mirror the native copy path, which rejects non-text payloads.
                onError("This item isn't text, so it can't be copied as text.");
                return;
            }
            await writeClipboardText(payload.text);
        } catch (caught) {
            onError(formatBackendError(caught));
        }
    }

    return (
        <YStack gap="$3" flex={1}>
            <XStack justify="space-between" items="center" gap="$2" flexWrap="wrap">
                <H2 size="$6">Clipboard</H2>
                <Button
                    icon={busy ? <Spinner /> : <Copy size={16} />}
                    onPress={addClipboardText}
                    disabled={busy}
                >
                    Add Current Clipboard
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
                                        <Text
                                            style={{
                                                fontFamily: isTextMimeType(item.mime_type)
                                                    ? "ui-monospace, SFMono-Regular, Menlo, monospace"
                                                    : undefined,
                                            }}
                                            numberOfLines={3}
                                        >
                                            {item.text}
                                        </Text>
                                        <Paragraph size="$2" color="#9aa4ad">
                                            {item.mime_type} - {formatRelativeTime(item.created_at)}
                                        </Paragraph>
                                    </YStack>
                                    <Button
                                        size="$3"
                                        icon={<Copy size={16} />}
                                        disabled={!isTextMimeType(item.mime_type)}
                                        onPress={() => void copyItem(item)}
                                    />
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
    const fileInputRef = useRef<HTMLInputElement | null>(null);
    const [busy, setBusy] = useState(false);
    const [viewerFile, setViewerFile] = useState<{ filename: string; content: string } | null>(
        null,
    );
    const nativeRuntime = isTauriRuntime();

    async function uploadFile(file: File) {
        setBusy(true);
        onError(null);
        try {
            const bytes = new Uint8Array(await file.arrayBuffer());
            const backend = await clipperBackend();
            await backend.uploadFileBytes(file.name, file.type, bytes);
            onState(await backend.getState());
        } catch (caught) {
            onError(formatBackendError(caught));
        } finally {
            setBusy(false);
            if (fileInputRef.current) fileInputRef.current.value = "";
        }
    }

    async function uploadNativeFile() {
        setBusy(true);
        onError(null);
        try {
            const backend = await clipperBackend();
            if (!backend.uploadFileFromDialog) throw new Error("Native file upload is unavailable");
            const uploadedId = await backend.uploadFileFromDialog();
            if (uploadedId) onState(await backend.getState());
        } catch (caught) {
            onError(formatBackendError(caught));
        } finally {
            setBusy(false);
        }
    }

    function pickUploadFile() {
        if (nativeRuntime) {
            void uploadNativeFile();
            return;
        }

        fileInputRef.current?.click();
    }

    async function downloadFile(file: FileItem) {
        onError(null);
        try {
            const backend = await clipperBackend();
            if (nativeRuntime && backend.downloadFileToDialog) {
                await backend.downloadFileToDialog(file.id, safeDownloadFilename(file.filename));
                return;
            }

            const bytes = await backend.downloadFileBytes(file.id);
            downloadBytes(safeDownloadFilename(file.filename), bytes, file.mime_type);
        } catch (caught) {
            onError(formatBackendError(caught));
        }
    }

    async function openViewer(file: FileItem) {
        onError(null);
        try {
            const backend = await clipperBackend();
            const bytes = await backend.downloadFileBytes(file.id);
            const content = new TextDecoder().decode(bytes);
            setViewerFile({ filename: file.filename, content });
        } catch (caught) {
            onError(formatBackendError(caught));
        }
    }

    async function deleteFile(file: FileItem) {
        onError(null);
        try {
            const backend = await clipperBackend();
            await backend.deleteFile(file.id);
            onState(await backend.getState());
        } catch (caught) {
            onError(formatBackendError(caught));
        }
    }

    return (
        <>
            {viewerFile && (
                <FileViewerOverlay
                    filename={viewerFile.filename}
                    content={viewerFile.content}
                    onClose={() => setViewerFile(null)}
                />
            )}
            <YStack gap="$3" flex={1}>
                <input
                    ref={fileInputRef}
                    type="file"
                    aria-hidden="true"
                    tabIndex={-1}
                    style={{ display: "none" }}
                    onChange={(event) => {
                        const file = event.currentTarget.files?.item(0);
                        if (file) void uploadFile(file);
                    }}
                />

                <XStack justify="space-between" items="center" gap="$2" flexWrap="wrap">
                    <H2 size="$6">Files</H2>
                    <Button
                        icon={busy ? <Spinner /> : <FileUp size={16} />}
                        onPress={pickUploadFile}
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
                                                    {formatByteSize(file.blob_size)} -{" "}
                                                    {formatRelativeTime(file.created_at)}
                                                </Paragraph>
                                            </YStack>
                                        </XStack>
                                        <XStack gap="$1">
                                            {isTextMimeType(file.mime_type) && (
                                                <Button
                                                    size="$3"
                                                    icon={<Eye size={16} />}
                                                    onPress={() => void openViewer(file)}
                                                />
                                            )}
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
        </>
    );
}

function FileViewerOverlay({
    filename,
    content,
    onClose,
}: {
    filename: string;
    content: string;
    onClose: () => void;
}) {
    const [vimMode, setVimMode] = useState(
        () => globalThis.localStorage?.getItem(VIM_MODE_STORAGE_KEY) === "true",
    );

    useEffect(() => {
        function onKeyDown(event: KeyboardEvent) {
            if (event.key === "Escape") onClose();
        }

        // Capture phase so Escape closes the overlay even while the CodeMirror
        // editor (with vim bindings) has focus and would otherwise swallow it.
        globalThis.addEventListener("keydown", onKeyDown, true);
        return () => globalThis.removeEventListener("keydown", onKeyDown, true);
    }, [onClose]);

    function toggleVimMode() {
        setVimMode((previous) => {
            const next = !previous;
            globalThis.localStorage?.setItem(VIM_MODE_STORAGE_KEY, String(next));
            return next;
        });
    }

    return (
        <div
            style={{
                position: "fixed",
                inset: 0,
                zIndex: 1000,
                display: "flex",
                flexDirection: "column",
                background: "#101214",
            }}
        >
            <div
                style={{
                    display: "flex",
                    alignItems: "center",
                    justifyContent: "space-between",
                    gap: 12,
                    padding: "10px 16px",
                    background: "#171a1d",
                    borderBottom: "1px solid #252b31",
                }}
            >
                <span
                    style={{
                        color: "#e6e9ec",
                        fontWeight: 600,
                        overflow: "hidden",
                        textOverflow: "ellipsis",
                        whiteSpace: "nowrap",
                    }}
                >
                    {filename}
                </span>
                <XStack items="center" gap="$2">
                    <Button size="$3" theme={vimMode ? "blue" : undefined} onPress={toggleVimMode}>
                        Vim
                    </Button>
                    <Button size="$3" icon={<X size={16} />} onPress={onClose} />
                </XStack>
            </div>
            <div style={{ flex: 1, minHeight: 0 }}>
                <ErrorBoundary fallback={renderEditorError}>
                    <Suspense fallback={codeEditorFallback}>
                        <CodeEditor content={content} lang={filename} vimMode={vimMode} />
                    </Suspense>
                </ErrorBoundary>
            </div>
        </div>
    );
}

function CollabPanel({
    collabDocs,
    onState,
    onError,
}: {
    collabDocs: CollabItem[];
    onState: (state: AppState) => void;
    onError: (error: string | null) => void;
}) {
    const [, setLocation] = useLocation();
    const [busy, setBusy] = useState(false);

    async function createDoc() {
        setBusy(true);
        onError(null);
        try {
            const backend = await clipperBackend();
            const created = await backend.createCollabDoc();
            onState(await backend.getState());
            setLocation(`/collab/${created.id}`);
        } catch (caught) {
            onError(formatBackendError(caught));
        } finally {
            setBusy(false);
        }
    }

    async function copyLink(item: CollabItem) {
        onError(null);
        try {
            await writeClipboardText(shareLink(item.share_token));
        } catch (caught) {
            onError(formatBackendError(caught));
        }
    }

    async function deleteDoc(item: CollabItem) {
        onError(null);
        try {
            const backend = await clipperBackend();
            await backend.deleteCollabDoc(item.id);
            onState(await backend.getState());
        } catch (caught) {
            onError(formatBackendError(caught));
        }
    }

    return (
        <YStack gap="$3" flex={1}>
            <XStack justify="space-between" items="center" gap="$2" flexWrap="wrap">
                <H2 size="$6">Collab Docs</H2>
                <Button
                    icon={busy ? <Spinner /> : <FilePlus size={16} />}
                    onPress={() => void createDoc()}
                    disabled={busy}
                >
                    New Doc
                </Button>
            </XStack>

            {collabDocs.length === 0 ? (
                <EmptyState icon={<FileText size={28} />} title="No collab docs yet" />
            ) : (
                <ScrollView>
                    <YStack gap="$2" pb="$4">
                        {collabDocs.map((item) => (
                            <ListCard key={item.id}>
                                <XStack items="center" justify="space-between" gap="$3">
                                    <XStack
                                        items="center"
                                        gap="$3"
                                        flex={1}
                                        cursor="pointer"
                                        onPress={() => setLocation(`/collab/${item.id}`)}
                                    >
                                        <FileCode size={22} color="#6fb4ff" />
                                        <YStack flex={1} gap="$1">
                                            <Text numberOfLines={1}>{collabTitle(item.id)}</Text>
                                            <Paragraph size="$2" color="#9aa4ad">
                                                {formatRelativeTime(item.created_at)}
                                            </Paragraph>
                                        </YStack>
                                    </XStack>
                                    <XStack gap="$1">
                                        <Button
                                            size="$3"
                                            icon={<Copy size={16} />}
                                            onPress={() => void copyLink(item)}
                                        />
                                        <Button
                                            size="$3"
                                            icon={<Trash2 size={16} color="#ff6b6b" />}
                                            onPress={() => void deleteDoc(item)}
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

function CollabDocView({ id, onError }: { id: string; onError: (error: string | null) => void }) {
    const [, setLocation] = useLocation();
    const [meta, setMeta] = useState<CollabItem | null>(null);
    const [loading, setLoading] = useState(true);
    const [copied, setCopied] = useState(false);

    useEffect(() => {
        let cancelled = false;
        setLoading(true);
        onError(null);

        async function load() {
            try {
                const backend = await clipperBackend();
                const loaded = await backend.getCollabDocMeta(id);
                if (!cancelled) setMeta(loaded);
            } catch (caught) {
                if (!cancelled) onError(formatBackendError(caught));
            } finally {
                if (!cancelled) setLoading(false);
            }
        }

        void load();

        return () => {
            cancelled = true;
        };
    }, [id, onError]);

    async function copyLink() {
        if (!meta) return;
        onError(null);
        try {
            await writeClipboardText(shareLink(meta.share_token));
            setCopied(true);
            globalThis.setTimeout(() => setCopied(false), 2000);
        } catch (caught) {
            onError(formatBackendError(caught));
        }
    }

    return (
        <YStack gap="$3" flex={1}>
            <XStack justify="space-between" items="center" gap="$2" flexWrap="wrap">
                <XStack items="center" gap="$3" flex={1}>
                    <Button
                        size="$3"
                        icon={<ArrowLeft size={16} />}
                        onPress={() => setLocation("/collab")}
                    />
                    <H2 size="$6">{collabTitle(id)}</H2>
                </XStack>
            </XStack>

            {loading ? (
                <EmptyState icon={<Spinner />} title="Loading doc..." />
            ) : meta ? (
                <YStack gap="$3" flex={1}>
                    <ListCard>
                        <XStack items="center" justify="space-between" gap="$3" flexWrap="wrap">
                            <YStack flex={1} gap="$1">
                                <Paragraph size="$2" color="#9aa4ad">
                                    Share link
                                </Paragraph>
                                <Text
                                    numberOfLines={1}
                                    style={{
                                        fontFamily:
                                            "ui-monospace, SFMono-Regular, Menlo, monospace",
                                    }}
                                >
                                    {shareLink(meta.share_token)}
                                </Text>
                            </YStack>
                            <Button
                                size="$3"
                                icon={<Copy size={16} />}
                                onPress={() => void copyLink()}
                            >
                                {copied ? "Copied" : "Copy link"}
                            </Button>
                        </XStack>
                    </ListCard>
                    <Card
                        flex={1}
                        bg="#171a1d"
                        overflow="hidden"
                        style={{ borderColor: "#252b31", borderWidth: 1 }}
                    >
                        <ErrorBoundary fallback={renderEditorError}>
                            <Suspense fallback={codeEditorFallback}>
                                <CodeEditor content="" lang="markdown" />
                            </Suspense>
                        </ErrorBoundary>
                    </Card>
                </YStack>
            ) : (
                <EmptyState icon={<FileText size={28} />} title="Doc unavailable" />
            )}
        </YStack>
    );
}

function DevicesPanel({ onError }: { onError: (error: string | null) => void }) {
    const [devices, setDevices] = useState<DeviceInfo[] | null>(null);
    const [busy, setBusy] = useState(false);

    const loadDevices = useCallback(async () => {
        setBusy(true);
        onError(null);
        try {
            const backend = await clipperBackend();
            setDevices(await backend.listDevices());
        } catch (caught) {
            onError(formatBackendError(caught));
        } finally {
            setBusy(false);
        }
    }, [onError]);

    useEffect(() => {
        void loadDevices();
    }, [loadDevices]);

    async function removeDevice(device: DeviceInfo) {
        onError(null);
        try {
            const backend = await clipperBackend();
            await backend.removeDevice(device.id);
            await loadDevices();
        } catch (caught) {
            onError(formatBackendError(caught));
        }
    }

    return (
        <YStack gap="$3" flex={1}>
            <XStack justify="space-between" items="center" gap="$2" flexWrap="wrap">
                <H2 size="$6">Devices</H2>
                <Button
                    icon={busy ? <Spinner /> : <RefreshCw size={16} />}
                    onPress={() => void loadDevices()}
                    disabled={busy}
                >
                    Refresh
                </Button>
            </XStack>

            <Paragraph size="$2" color="#9aa4ad">
                Removing a device signs it out everywhere and revokes its access. The objects it
                shared are kept.
            </Paragraph>

            {devices === null ? (
                <EmptyState icon={<Spinner />} title="Loading devices..." />
            ) : devices.length === 0 ? (
                <EmptyState icon={<Smartphone size={28} />} title="No devices" />
            ) : (
                <ScrollView>
                    <YStack gap="$2" pb="$4">
                        {devices.map((device) => (
                            <ListCard key={device.id}>
                                <XStack items="center" justify="space-between" gap="$3">
                                    <XStack items="center" gap="$3" flex={1}>
                                        <Smartphone size={22} color="#6fb4ff" />
                                        <YStack flex={1} gap="$1">
                                            <XStack items="center" gap="$2" flexWrap="wrap">
                                                <Text numberOfLines={1}>{device.name}</Text>
                                                {device.is_current && (
                                                    <Paragraph size="$1" color="#6fb4ff">
                                                        This device
                                                    </Paragraph>
                                                )}
                                            </XStack>
                                            <Paragraph size="$2" color="#9aa4ad">
                                                {device.platform} - last seen{" "}
                                                {formatRelativeTime(device.last_seen_at)}
                                            </Paragraph>
                                        </YStack>
                                    </XStack>
                                    {!device.is_current && (
                                        <Button
                                            size="$3"
                                            icon={<Trash2 size={16} color="#ff6b6b" />}
                                            onPress={() => void removeDevice(device)}
                                        />
                                    )}
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
            <Label>{label}</Label>
            {children}
        </YStack>
    );
}

function ListCard({ children }: { children: ReactNode }) {
    return (
        <Card p="$3" bg="#171a1d" style={{ borderColor: "#252b31", borderWidth: 1 }}>
            {children}
        </Card>
    );
}

function EmptyState({ icon, title }: { icon: ReactNode; title: string }) {
    return (
        <YStack flex={1} items="center" justify="center" gap="$3" p="$6">
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
        <YStack minH="100vh" items="center" justify="center" gap="$3" p="$5">
            {loading && <Spinner size="large" />}
            <H2>{title}</H2>
            {message && (
                <Paragraph maxW={620} text="center" color="#9aa4ad">
                    {message}
                </Paragraph>
            )}
        </YStack>
    );
}

function ConnectionBadge({ status }: { status: AppState["connection_status"] }) {
    const color =
        status === "Connected" ? "#3ddc84" : status === "Connecting" ? "#f2c94c" : "#9099a1";
    return (
        <XStack items="center" gap="$2" px="$2" py="$1" rounded="$2" bg="#22282e">
            <YStack width={8} height={8} rounded={999} bg={color} />
            <Text fontSize={12} color="#9aa4ad">
                {status}
            </Text>
        </XStack>
    );
}

function isTextMimeType(mimeType: string): boolean {
    return mimeType.toLowerCase().split(";")[0]?.trim().startsWith("text/") ?? false;
}

// Placeholder collab-doc title. The real title lives inside the Y.Doc state and
// is wired up in Phase 3; until then the doc is identified by a short id prefix.
function collabTitle(id: string): string {
    return `Doc · ${id.slice(0, 8)}`;
}

// The public share URL a doc's share token resolves to. The `/s/:share_token`
// route itself is a Phase 3b addition; the link is shown and copyable now.
function shareLink(shareToken: string): string {
    const origin = typeof window === "undefined" ? "" : window.location.origin;
    return `${origin}/s/${shareToken}`;
}

function formatRelativeTime(value: string): string {
    const date = Date.parse(value);
    if (Number.isNaN(date)) return value;

    const seconds = Math.max(0, Math.round((Date.now() - date) / 1000));
    if (seconds < 60) return "just now";

    const minutes = Math.round(seconds / 60);
    if (minutes < 60) return `${minutes}m ago`;

    const hours = Math.round(minutes / 60);
    if (hours < 24) return `${hours}h ago`;

    const days = Math.round(hours / 24);
    if (days < 30) return `${days}d ago`;

    return new Date(date).toLocaleDateString();
}

function formatByteSize(rawBytes: number): string {
    // Defensive clamp: the web wasm path passes serde_wasm_bindgen output straight
    // into this typed field, so a buggy same-user peer (meta.size) or the
    // untrusted server (ciphertext_size fallback) could drive a negative / NaN /
    // absurd value here. The mobile bridge already clamps its i64 size fields.
    const bytes = Number.isFinite(rawBytes)
        ? Math.max(0, Math.min(rawBytes, Number.MAX_SAFE_INTEGER))
        : 0;
    if (bytes < 1024) return `${bytes} B`;
    const units = ["KiB", "MiB", "GiB"];
    let value = bytes / 1024;
    for (const unit of units) {
        if (value < 1024) return `${value.toFixed(value < 10 ? 1 : 0)} ${unit}`;
        value /= 1024;
    }
    return `${value.toFixed(1)} TiB`;
}

function safeDownloadFilename(filename: string): string {
    const cleaned = filename.replace(/[\\/:*?"<>|]/g, "_").trim();
    return cleaned.length > 0 ? cleaned : "clipper-download";
}

function downloadBytes(filename: string, bytes: Uint8Array, mimeType: string) {
    const data =
        bytes.buffer instanceof ArrayBuffer
            ? bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength)
            : new Uint8Array(bytes).buffer;
    const blob = new Blob([data], {
        type: mimeType || "application/octet-stream",
    });
    const url = URL.createObjectURL(blob);
    const link = document.createElement("a");
    link.href = url;
    link.download = filename;
    document.body.append(link);
    link.click();
    link.remove();
    URL.revokeObjectURL(url);
}
