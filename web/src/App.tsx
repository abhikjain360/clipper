import { Button, Input } from "./tamagui.config";
import { palette } from "@clipper/shared";
import {
    ArrowLeft,
    CalendarClock,
    Clipboard,
    CookingPot,
    Copy,
    Download,
    Dumbbell,
    Eye,
    FileCode,
    FilePlus,
    FileText,
    FileUp,
    Files,
    Folder,
    LogOut,
    Menu,
    PanelLeftClose,
    RefreshCw,
    Smartphone,
    Trash2,
    X,
} from "lucide-react";
import { clipboardImage, clipboardBytes } from "../../packages/shared/src/clipboard";
import { filePreview, filePreviewType } from "../../packages/shared/src/file-preview";
import { cardEvents } from "./card-events";
import {
    Suspense,
    lazy,
    useCallback,
    useEffect,
    useRef,
    useState,
    type FormEvent,
    type KeyboardEvent as ReactKeyboardEvent,
    type ReactNode,
} from "react";
import { Route, Switch, useLocation } from "wouter";
import { Card, H1, H2, Label, Paragraph, Spinner, Text, XStack, YStack } from "tamagui";
import {
    clearSessionResume,
    clipperBackend,
    defaultServerUrl,
    formatBackendError,
    isTauriRuntime,
    readClipboardText,
    resolveServerUrl,
    resumeSession,
    saveSessionResume,
    writeClipboardText,
} from "./backend";
import type {
    AppState,
    ClipboardItem,
    CollabItem,
    DeviceInfo,
    FileItem,
    RunningWorkView,
} from "@clipper/shared";
import { ErrorBoundary } from "./ErrorBoundary";
import { SchedulePanel } from "./SchedulePanel";
import { GymPanel } from "./gym/GymPanel";
import { KitchenPanel } from "./kitchen/KitchenPanel";

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
            color: palette.secondary,
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
            <div style={{ color: palette.danger, fontWeight: 600, marginBottom: 6 }}>
                Editor failed to load
            </div>
            <div
                style={{
                    color: palette.secondary,
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

export default function App() {
    // The public share page is reachable without a session, so it sits above the
    // engine startup / login gate — opening a share link never boots the
    // authenticated client.
    return (
        <Switch>
            <Route path="/s/:token">{(params) => <SharePage token={params.token} />}</Route>
            <Route>
                <MainApp />
            </Route>
        </Switch>
    );
}

function MainApp() {
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
            <LoginScreen
                initialUsername={state.saved_profile?.username ?? ""}
                initialServerUrl={state.saved_profile?.server_url ?? ""}
                onState={setState}
            />
        );
    }

    return <HomeScreen state={state} onState={setState} />;
}

function LoginScreen({
    initialUsername,
    initialServerUrl,
    onState,
}: {
    initialUsername: string;
    initialServerUrl: string;
    onState: (state: AppState) => void;
}) {
    const [mode, setMode] = useState<"login" | "register">("login");
    const [serverUrl, setServerUrl] = useState(initialServerUrl);
    const [username, setUsername] = useState(initialUsername);
    const [passphrase, setPassphrase] = useState("");
    const [accessKey, setAccessKey] = useState("");
    const [busy, setBusy] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const busyRef = useRef(false);

    useEffect(() => {
        if (initialServerUrl) {
            setServerUrl(initialServerUrl);
            return;
        }
        const envUrl = import.meta.env.VITE_SERVER_URL as string | undefined;
        if (envUrl) {
            setServerUrl(envUrl);
            return;
        }
        let cancelled = false;
        void defaultServerUrl()
            .catch(() => "http://127.0.0.1:8787")
            .then((url) => {
                if (!cancelled) setServerUrl(url);
            });
        return () => {
            cancelled = true;
        };
    }, [initialServerUrl]);

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
            // Persist a resume blob — server token + derived keys, never the
            // passphrase — so a reload skips re-login. Browser-only; under Tauri
            // `sessionResumeMaterial()` returns null and nothing is stored.
            const material = await backend.sessionResumeMaterial();
            saveSessionResume(material, username, "", serverUrl);
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
            <Card width="100%" maxW={460} p="$5" bg={palette.cardFill}>
                <form onSubmit={submit}>
                    <YStack gap="$4">
                        <YStack gap="$2">
                            <H1 size="$9">Clipper</H1>
                            <Paragraph color={palette.secondary}>
                                Encrypted clipboard and file sync
                            </Paragraph>
                        </YStack>

                        <XStack gap="$2">
                            <Button
                                type="button"
                                flex={1}
                                selected={mode === "login"}
                                onPress={() => setMode("login")}
                            >
                                Login
                            </Button>
                            <Button
                                type="button"
                                flex={1}
                                selected={mode === "register"}
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
                                    autoComplete="off"
                                    spellCheck={false}
                                    secureTextEntry
                                    type="password"
                                    onChangeText={setAccessKey}
                                />
                            </Field>
                        )}
                        <Field label="Passphrase">
                            <Input
                                value={passphrase}
                                secureTextEntry
                                type="password"
                                autoComplete="new-password"
                                spellCheck={false}
                                onChangeText={setPassphrase}
                            />
                        </Field>

                        {error && <Paragraph color={palette.danger}>{error}</Paragraph>}

                        <Button
                            type="submit"
                            tone="accent"
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
    const [runningWork, setRunningWork] = useState<RunningWorkView[] | null>(null);
    const [loggingOut, setLoggingOut] = useState(false);

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

    async function logout(cancelRunningWork = false) {
        setError(null);
        setLoggingOut(true);
        try {
            const backend = await clipperBackend();
            const outcome = await backend.logout(cancelRunningWork);
            if (outcome.status === "work_running") {
                setRunningWork(outcome.work);
                setNavExpanded(true);
                return;
            }
            setRunningWork(null);
            clearSessionResume();
            onState(await backend.getState());
        } catch (caught) {
            setError(formatBackendError(caught));
        } finally {
            setLoggingOut(false);
        }
    }

    const [navExpanded, setNavExpanded] = useState(false);
    const mobileNav = useRef<HTMLDialogElement>(null);
    const destinations = [
        { path: "/", label: "Clipboard", icon: Clipboard },
        { path: "/files", label: "Files", icon: Folder },
        { path: "/collab", label: "Collab Docs", icon: FileText },
        { path: "/schedule", label: "Schedule", icon: CalendarClock },
        { path: "/kitchen", label: "Kitchen", icon: CookingPot },
        { path: "/gym", label: "Gym", icon: Dumbbell },
        { path: "/devices", label: "Devices", icon: Smartphone },
    ];
    const navigation = (expanded: boolean, mobile = false) => (
        <>
            <button
                className="nav-collapse-toggle"
                aria-label={
                    mobile
                        ? "Close navigation"
                        : navExpanded
                          ? "Collapse navigation"
                          : "Expand navigation"
                }
                onClick={() => (mobile ? mobileNav.current?.close() : setNavExpanded(!navExpanded))}
            >
                {mobile ? (
                    <X size={20} />
                ) : expanded ? (
                    <PanelLeftClose size={20} />
                ) : (
                    <Menu size={20} />
                )}
            </button>
            <nav aria-label="Main navigation" className="nav-destinations">
                {destinations.map(({ path, label, icon: Icon }) => (
                    <Button
                        key={path}
                        aria-label={label}
                        aria-current={
                            (path === "/" ? location === path : location.startsWith(path))
                                ? "page"
                                : undefined
                        }
                        selected={path === "/" ? location === path : location.startsWith(path)}
                        icon={<Icon size={20} />}
                        justify={expanded ? "flex-start" : "center"}
                        onPress={() => {
                            setLocation(path);
                            if (mobile) mobileNav.current?.close();
                        }}
                    >
                        {expanded ? label : null}
                    </Button>
                ))}
            </nav>
            <div className="nav-bottom">
                <Button
                    aria-label="Refresh"
                    icon={busy ? <Spinner /> : <RefreshCw size={20} />}
                    onPress={refresh}
                    disabled={busy}
                    justify={expanded ? "flex-start" : "center"}
                >
                    {expanded ? "Refresh" : null}
                </Button>
                <Button
                    aria-label="Logout"
                    icon={<LogOut size={20} />}
                    onPress={() => void logout(false)}
                    disabled={loggingOut}
                    justify={expanded ? "flex-start" : "center"}
                >
                    {expanded ? "Logout" : null}
                </Button>
                {runningWork && (
                    <Card p="$3" gap="$2" borderWidth={0} aria-label="Running work">
                        <Paragraph>Work is still running</Paragraph>
                        {runningWork.map((work, index) => (
                            <Text key={index}>{work.label}</Text>
                        ))}
                        <Button disabled={loggingOut} onPress={() => setRunningWork(null)}>
                            Wait
                        </Button>
                        <Button
                            tone="danger"
                            height="auto"
                            py="$2"
                            disabled={loggingOut}
                            onPress={() => void logout(true)}
                        >
                            <Text>Cancel them and log out</Text>
                        </Button>
                    </Card>
                )}
                <div className="nav-brand" aria-label={`Clipper: ${state.connection_status}`}>
                    <Clipboard size={22} aria-hidden="true" />
                    {expanded && <span>Clipper</span>}
                </div>
                {expanded && <ConnectionBadge status={state.connection_status} />}
            </div>
        </>
    );

    return (
        <div className={`app-shell${navExpanded ? " nav-expanded" : ""}`}>
            <aside className="desktop-navigation">{navigation(navExpanded)}</aside>
            <button
                className="mobile-nav-toggle"
                aria-label="Open navigation"
                onClick={() => mobileNav.current?.showModal()}
            >
                <Menu size={22} />
            </button>
            <dialog
                className="mobile-nav-dialog"
                ref={mobileNav}
                aria-label="Navigation"
                onClick={(event) => {
                    if (event.target === event.currentTarget) mobileNav.current?.close();
                }}
            >
                <div className="mobile-navigation">{navigation(true, true)}</div>
            </dialog>
            <main className="app-main">
                <YStack width="100%" self="center" p="$3" gap="$3" flex={1}>
                    {error && <Paragraph color={palette.danger}>{error}</Paragraph>}

                    <Switch>
                        <Route path="/files">
                            <FilesPanel files={state.files} onState={onState} onError={setError} />
                        </Route>
                        <Route path="/collab/:id">
                            {(params) => (
                                <CollabDocView
                                    id={params.id}
                                    displayName={state.session?.username ?? "You"}
                                    onError={setError}
                                />
                            )}
                        </Route>
                        <Route path="/collab">
                            <CollabPanel
                                collabDocs={state.collab_docs}
                                onState={onState}
                                onError={setError}
                            />
                        </Route>
                        <Route path="/schedule">
                            <SchedulePanel
                                state={state}
                                items={state.schedule_items}
                                warnings={state.schedule_warnings}
                                sources={state.calendar_sources}
                                running={state.running_actual ?? null}
                                onState={onState}
                                onError={setError}
                            />
                        </Route>
                        <Route path="/kitchen/:id">
                            {(params) => (
                                <KitchenPanel
                                    key={params.id}
                                    id={params.id}
                                    state={state}
                                    onError={setError}
                                />
                            )}
                        </Route>
                        <Route path="/kitchen">
                            <KitchenPanel state={state} onError={setError} />
                        </Route>
                        <Route path="/gym">
                            <GymPanel state={state} onError={setError} />
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
            </main>
        </div>
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
    const deleteDialog = useRef<HTMLDialogElement>(null);
    const [deleting, setDeleting] = useState<ClipboardItem | null>(null);
    const [deleteBusy, setDeleteBusy] = useState(false);
    const [deleteError, setDeleteError] = useState<string | null>(null);
    const [viewing, setViewing] = useState<{
        id: string;
        content: string;
        image: string | null;
    } | null>(null);

    useEffect(() => {
        if (viewing && !items.some((item) => item.id === viewing.id)) setViewing(null);
    }, [items, viewing]);

    useEffect(() => {
        if (deleting) deleteDialog.current?.showModal();
        else deleteDialog.current?.close();
    }, [deleting]);

    async function viewItem(item: ClipboardItem) {
        onError(null);
        try {
            const payload = await (await clipperBackend()).clipboardPayload(item.id);
            const image = clipboardImage(payload.mimeType, payload.bytes);
            setViewing({
                id: item.id,
                content: payload.text ?? (image ? "" : clipboardBytes(payload.bytes)),
                image,
            });
        } catch (caught) {
            onError(formatBackendError(caught));
        }
    }

    async function deleteItem(item: ClipboardItem) {
        if (deleteBusy) return;
        setDeleteBusy(true);
        setDeleteError(null);
        onError(null);
        try {
            const backend = await clipperBackend();
            await backend.deleteClipboard(item.id);
            onState(await backend.getState());
            setDeleting(null);
        } catch (caught) {
            setDeleteError(formatBackendError(caught));
        } finally {
            setDeleteBusy(false);
        }
    }

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
            <dialog
                ref={deleteDialog}
                aria-label="Delete clipboard item"
                onCancel={(event) => {
                    if (deleteBusy) event.preventDefault();
                    else setDeleting(null);
                }}
                style={{
                    background: palette.cardFill,
                    color: palette.text,
                    border: "none",
                    borderRadius: 12,
                    padding: 24,
                }}
            >
                <YStack gap="$3">
                    <Paragraph>Delete permanently? This cannot be undone.</Paragraph>
                    {deleteError && <Paragraph color={palette.danger}>{deleteError}</Paragraph>}
                    <XStack justify="flex-end" gap="$2">
                        <Button disabled={deleteBusy} onPress={() => setDeleting(null)}>
                            Cancel
                        </Button>
                        <Button
                            disabled={deleteBusy}
                            icon={deleteBusy ? <Spinner /> : <Trash2 size={16} />}
                            onPress={() => {
                                if (deleting) void deleteItem(deleting);
                            }}
                        >
                            Delete
                        </Button>
                    </XStack>
                </YStack>
            </dialog>
            {viewing && (
                <FileViewerOverlay
                    filename="Clipboard"
                    content={viewing.content}
                    image={viewing.image}
                    onClose={() => setViewing(null)}
                />
            )}
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
                <div className="library-grid">
                    {items.map((item) => (
                        <ListCard key={item.id} onOpen={() => void viewItem(item)}>
                            <XStack items="center" justify="space-between" gap="$3">
                                <YStack flex={1} gap="$1">
                                    <Text
                                        style={{
                                            fontFamily: isTextMimeType(item.mime_type)
                                                ? "ui-monospace, SFMono-Regular, Menlo, monospace"
                                                : undefined,
                                        }}
                                        numberOfLines={6}
                                    >
                                        {item.text}
                                    </Text>
                                    <Paragraph size="$2" color={palette.secondary}>
                                        {item.mime_type} - {formatRelativeTime(item.created_at)}
                                    </Paragraph>
                                </YStack>
                                <XStack gap="$1" onPress={(event) => event.stopPropagation()}>
                                    <Button
                                        size="$3"
                                        aria-label="Copy clipboard item"
                                        icon={<Copy size={16} />}
                                        disabled={!isTextMimeType(item.mime_type)}
                                        onPress={(event) => {
                                            event.stopPropagation();
                                            void copyItem(item);
                                        }}
                                    />
                                    <Button
                                        size="$3"
                                        aria-label="Delete clipboard item"
                                        icon={<Trash2 size={16} color={palette.danger} />}
                                        onPress={(event) => {
                                            event.stopPropagation();
                                            setDeleteError(null);
                                            setDeleting(item);
                                        }}
                                    />
                                </XStack>
                            </XStack>
                        </ListCard>
                    ))}
                </div>
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
    const [viewerFile, setViewerFile] = useState<{
        file: FileItem;
        content: string;
        image?: string | null;
        details: boolean;
    } | null>(null);
    const nativeRuntime = isTauriRuntime();

    async function uploadFile(file: File) {
        setBusy(true);
        onError(null);
        try {
            if (file.size > 512 * 1024 * 1024) {
                throw new Error("Files larger than 512 MiB cannot be uploaded.");
            }
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
            const preview = await filePreview(
                file,
                async () => (await clipperBackend()).downloadFileBytes(file.id),
                (bytes) => new TextDecoder().decode(bytes),
            );
            setViewerFile({
                file,
                details: !preview,
                content:
                    preview?.content ??
                    `${file.mime_type}\n${formatByteSize(file.blob_size)}\n${formatRelativeTime(file.created_at)}`,
                image: preview?.image,
            });
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
                    filename={viewerFile.file.filename}
                    content={viewerFile.content}
                    image={viewerFile.image}
                    details={viewerFile.details}
                    onDownload={() => void downloadFile(viewerFile.file)}
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
                    <div className="library-grid">
                        {files.map((file) => (
                            <ListCard key={file.id} onOpen={() => void openViewer(file)}>
                                <XStack items="center" justify="space-between" gap="$3">
                                    <XStack items="center" gap="$3" flex={1}>
                                        <Files size={22} color={palette.accent} />
                                        <YStack flex={1} gap="$1">
                                            <Text numberOfLines={1}>{file.filename}</Text>
                                            <Paragraph size="$2" color={palette.secondary}>
                                                {formatByteSize(file.blob_size)} -{" "}
                                                {formatRelativeTime(file.created_at)}
                                            </Paragraph>
                                        </YStack>
                                    </XStack>
                                    <XStack gap="$1">
                                        {filePreviewType(file) && (
                                            <Button
                                                size="$3"
                                                aria-label={`Preview ${file.filename}`}
                                                icon={<Eye size={16} />}
                                                onPress={(event) => {
                                                    event.stopPropagation();
                                                    void openViewer(file);
                                                }}
                                            />
                                        )}
                                        <Button
                                            size="$3"
                                            aria-label={`Download ${file.filename}`}
                                            icon={<Download size={16} />}
                                            onPress={(event) => {
                                                event.stopPropagation();
                                                void downloadFile(file);
                                            }}
                                        />
                                        <Button
                                            size="$3"
                                            aria-label={`Delete ${file.filename}`}
                                            icon={<Trash2 size={16} color={palette.danger} />}
                                            onPress={(event) => {
                                                event.stopPropagation();
                                                void deleteFile(file);
                                            }}
                                        />
                                    </XStack>
                                </XStack>
                            </ListCard>
                        ))}
                    </div>
                )}
            </YStack>
        </>
    );
}

function FileViewerOverlay({
    filename,
    content,
    image,
    details,
    onDownload,
    onClose,
}: {
    filename: string;
    content: string;
    image?: string | null;
    details?: boolean;
    onDownload?: () => void;
    onClose: () => void;
}) {
    useEffect(() => {
        function onKeyDown(event: KeyboardEvent) {
            if (event.key === "Escape") onClose();
        }

        // Capture phase so Escape closes the overlay even while the CodeMirror
        // editor (with vim bindings) has focus and would otherwise swallow it.
        globalThis.addEventListener("keydown", onKeyDown, true);
        return () => globalThis.removeEventListener("keydown", onKeyDown, true);
    }, [onClose]);

    return (
        <div
            style={{
                position: "fixed",
                inset: 0,
                zIndex: 1000,
                display: "flex",
                flexDirection: "column",
                background: palette.pageFill,
            }}
        >
            <div
                style={{
                    display: "flex",
                    alignItems: "center",
                    justifyContent: "space-between",
                    gap: 12,
                    padding: "10px 16px",
                    background: palette.cardFill,
                    border: "none",
                }}
            >
                <span
                    style={{
                        color: palette.text,
                        fontWeight: 600,
                        overflow: "hidden",
                        textOverflow: "ellipsis",
                        whiteSpace: "nowrap",
                    }}
                >
                    {filename}
                </span>
                <XStack gap="$2">
                    {onDownload && (
                        <Button size="$3" icon={<Download size={16} />} onPress={onDownload}>
                            Download
                        </Button>
                    )}
                    <Button size="$3" icon={<X size={16} />} onPress={onClose} />
                </XStack>
            </div>
            <div style={{ flex: 1, minHeight: 0 }}>
                {details ? (
                    <Paragraph p="$4" whiteSpace="pre-wrap">
                        {content}
                    </Paragraph>
                ) : image ? (
                    <img
                        src={image}
                        alt={filename}
                        style={{ width: "100%", height: "100%", objectFit: "contain" }}
                    />
                ) : (
                    <ErrorBoundary fallback={renderEditorError}>
                        <Suspense fallback={codeEditorFallback}>
                            <CodeEditor content={content} lang={filename} />
                        </Suspense>
                    </ErrorBoundary>
                )}
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
        const link = shareLink(item);
        if (!link) {
            onError(SHARE_LINK_UNAVAILABLE);
            return;
        }
        try {
            await writeClipboardText(link);
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
                <div className="library-grid">
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
                                    <FileCode size={22} color={palette.accent} />
                                    <YStack flex={1} gap="$1">
                                        <Text numberOfLines={1}>{collabTitle(item)}</Text>
                                        <Paragraph size="$2" color={palette.secondary}>
                                            {formatRelativeTime(item.created_at)}
                                        </Paragraph>
                                    </YStack>
                                </XStack>
                                <XStack gap="$1">
                                    <Button
                                        size="$3"
                                        aria-label={`Copy link to ${collabTitle(item)}`}
                                        icon={<Copy size={16} />}
                                        onPress={() => void copyLink(item)}
                                    />
                                    <Button
                                        size="$3"
                                        aria-label={`Delete ${collabTitle(item)}`}
                                        icon={<Trash2 size={16} color={palette.danger} />}
                                        onPress={() => void deleteDoc(item)}
                                    />
                                </XStack>
                            </XStack>
                        </ListCard>
                    ))}
                </div>
            )}
        </YStack>
    );
}

function CollabDocView({
    id,
    displayName,
    onError,
}: {
    id: string;
    displayName: string;
    onError: (error: string | null) => void;
}) {
    const [, setLocation] = useLocation();
    const [meta, setMeta] = useState<CollabItem | null>(null);
    const [serverUrl, setServerUrl] = useState<string | null>(null);
    const [loading, setLoading] = useState(true);
    const [copied, setCopied] = useState(false);
    const [renaming, setRenaming] = useState(false);
    // Held in a ref so a second copy restarts the flash instead of stacking
    // timers (which would clear the label 2s after the *first* click).
    const copiedTimer = useRef<ReturnType<typeof globalThis.setTimeout> | null>(null);

    useEffect(
        () => () => {
            if (copiedTimer.current !== null) globalThis.clearTimeout(copiedTimer.current);
        },
        [],
    );

    useEffect(() => {
        let cancelled = false;
        setLoading(true);
        onError(null);

        async function load() {
            try {
                const backend = await clipperBackend();
                // The share token (for the WS credential) comes from the doc meta;
                // the server base URL is where the live-sync socket connects.
                const [loaded, resolvedServerUrl] = await Promise.all([
                    backend.getCollabDocMeta(id),
                    resolveServerUrl(),
                ]);
                if (!cancelled) {
                    setMeta(loaded);
                    setServerUrl(resolvedServerUrl);
                }
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
        const link = shareLink(meta);
        if (!link) {
            onError(SHARE_LINK_UNAVAILABLE);
            return;
        }
        try {
            await writeClipboardText(link);
            setCopied(true);
            if (copiedTimer.current !== null) globalThis.clearTimeout(copiedTimer.current);
            copiedTimer.current = globalThis.setTimeout(() => setCopied(false), 2000);
        } catch (caught) {
            onError(formatBackendError(caught));
        }
    }

    // Commit a rename. The server normalizes the title (trim) and returns the
    // stored value, so the input is re-seeded from the response rather than from
    // what was typed.
    async function saveTitle(next: string) {
        if (!meta) return;
        onError(null);
        setRenaming(true);
        try {
            const backend = await clipperBackend();
            setMeta(await backend.renameCollabDoc(id, next));
        } catch (caught) {
            onError(formatBackendError(caught));
        } finally {
            setRenaming(false);
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
                    {meta ? (
                        <TitleField
                            title={meta.title}
                            placeholder={collabTitle(meta)}
                            busy={renaming}
                            onSave={(next) => void saveTitle(next)}
                        />
                    ) : (
                        <H2 size="$6">Collab doc</H2>
                    )}
                </XStack>
            </XStack>

            {loading ? (
                <EmptyState icon={<Spinner />} title="Loading doc..." />
            ) : meta ? (
                <YStack gap="$3" flex={1}>
                    <ListCard>
                        <XStack items="center" justify="space-between" gap="$3" flexWrap="wrap">
                            <YStack flex={1} gap="$1">
                                <Paragraph size="$2" color={palette.secondary}>
                                    Share link
                                </Paragraph>
                                <Text
                                    numberOfLines={1}
                                    color={shareLink(meta) ? undefined : palette.secondary}
                                    style={{
                                        fontFamily: shareLink(meta)
                                            ? "ui-monospace, SFMono-Regular, Menlo, monospace"
                                            : undefined,
                                    }}
                                >
                                    {shareLink(meta) ?? SHARE_LINK_UNAVAILABLE}
                                </Text>
                            </YStack>
                            <Button
                                size="$3"
                                icon={<Copy size={16} />}
                                disabled={!shareLink(meta)}
                                onPress={() => void copyLink()}
                            >
                                {copied ? "Copied" : "Copy link"}
                            </Button>
                        </XStack>
                    </ListCard>
                    <Card flex={1} bg={palette.cardFill} overflow="hidden">
                        {serverUrl ? (
                            <ErrorBoundary fallback={renderEditorError}>
                                <Suspense fallback={codeEditorFallback}>
                                    <CodeEditor
                                        collab={{
                                            objectId: id,
                                            shareToken: meta.share_token,
                                            serverUrl,
                                            displayName,
                                        }}
                                    />
                                </Suspense>
                            </ErrorBoundary>
                        ) : (
                            codeEditorFallback
                        )}
                    </Card>
                </YStack>
            ) : (
                <EmptyState icon={<FileText size={28} />} title="Doc unavailable" />
            )}
        </YStack>
    );
}

// The collab doc's title, editable in place. The committed value is the source of
// truth: the local draft is re-seeded whenever the saved title changes (a rename
// from another device arrives over the event stream), except while the field is
// focused, so a remote update cannot overwrite what is being typed.
function TitleField({
    title,
    placeholder,
    busy,
    onSave,
}: {
    title: string;
    placeholder: string;
    busy: boolean;
    onSave: (next: string) => void;
}) {
    const [draft, setDraft] = useState(title);
    const [editing, setEditing] = useState(false);
    // Set by Escape so the blur it triggers discards the draft instead of
    // saving it. A ref, not state, because `commit` runs in the same tick as
    // the key handler and would not see a state update yet.
    const cancelled = useRef(false);

    // Also held while `busy`: committing blurs the field, so without that guard
    // the draft would snap back to the pre-save title for as long as the request
    // is in flight, then jump forward again — and a failed save would silently
    // discard what the user typed.
    useEffect(() => {
        if (!editing && !busy) setDraft(title);
    }, [title, editing, busy]);

    function commit() {
        setEditing(false);
        if (cancelled.current) {
            cancelled.current = false;
            setDraft(title);
            return;
        }
        if (draft.trim() === title.trim()) return;
        onSave(draft);
    }

    return (
        <XStack items="center" gap="$2" flex={1}>
            <Input
                flex={1}
                maxW={480}
                size="$4"
                value={draft}
                placeholder={placeholder}
                aria-label="Document title"
                bg={palette.inputFill}
                borderWidth={0}
                fontSize={20}
                fontWeight="600"
                onFocus={() => setEditing(true)}
                onChangeText={setDraft}
                onBlur={commit}
                onKeyDown={(event: ReactKeyboardEvent<HTMLInputElement>) => {
                    // Tamagui's web `Input` is `styled(View)`, not
                    // react-native-web's `TextInput`, so React Native's
                    // `onKeyPress`/`onSubmitEditing` callbacks and its
                    // blur-on-submit behaviour never fire here — this is a
                    // plain DOM key event. Both keys commit through blur so
                    // there is a single save path.
                    if (event.key === "Enter") {
                        event.preventDefault();
                        event.currentTarget.blur();
                    } else if (event.key === "Escape") {
                        // Without this every way out of an edit is a save.
                        cancelled.current = true;
                        event.currentTarget.blur();
                    }
                }}
            />
            {busy ? <Spinner size="small" /> : null}
        </XStack>
    );
}

// Public, unauthenticated editor for a share link (`/s/:token`). It resolves the
// token to a document id via the public meta endpoint, then live-edits over the
// same Y-sync WebSocket the owner uses — no account, no nav chrome.
function SharePage({ token }: { token: string }) {
    const [info, setInfo] = useState<{ objectId: string; serverUrl: string } | null>(null);
    const [error, setError] = useState<string | null>(null);

    useEffect(() => {
        let cancelled = false;

        async function load() {
            try {
                const serverUrl = await resolveServerUrl();
                const base = serverUrl.replace(/\/$/, "");
                const response = await fetch(`${base}/api/s/${encodeURIComponent(token)}/meta`);
                if (!response.ok) {
                    throw new Error(
                        response.status === 404
                            ? "This share link is invalid or no longer exists."
                            : `Could not open the document (HTTP ${response.status}).`,
                    );
                }
                const meta = (await response.json()) as { object_id?: unknown } | null;
                if (typeof meta?.object_id !== "string" || meta.object_id.length === 0) {
                    throw new Error("This share link is invalid or no longer exists.");
                }
                if (!cancelled) setInfo({ objectId: meta.object_id, serverUrl });
            } catch (caught) {
                if (!cancelled) setError(caught instanceof Error ? caught.message : String(caught));
            }
        }

        void load();

        return () => {
            cancelled = true;
        };
    }, [token]);

    if (error) {
        return <CenteredStatus title="Can't open document" message={error} />;
    }
    if (!info) {
        return <CenteredStatus title="Opening shared document" loading />;
    }

    return (
        <YStack minH="100vh" bg={palette.pageFill}>
            <YStack flex={1} style={{ minHeight: 0 }}>
                <ErrorBoundary fallback={renderEditorError}>
                    <Suspense fallback={codeEditorFallback}>
                        <CodeEditor
                            collab={{
                                objectId: info.objectId,
                                shareToken: token,
                                serverUrl: info.serverUrl,
                                displayName: "Guest",
                            }}
                        />
                    </Suspense>
                </ErrorBoundary>
            </YStack>
            <XStack justify="center" py="$2">
                <Text fontSize={12} color={palette.secondary}>
                    Made with Clipper
                </Text>
            </XStack>
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

            <Paragraph size="$2" color={palette.secondary}>
                Removing a device signs it out everywhere and revokes its access. The objects it
                shared are kept.
            </Paragraph>

            {devices === null ? (
                <EmptyState icon={<Spinner />} title="Loading devices..." />
            ) : devices.length === 0 ? (
                <EmptyState icon={<Smartphone size={28} />} title="No devices" />
            ) : (
                <div className="library-grid">
                    {devices.map((device) => (
                        <ListCard key={device.id}>
                            <XStack items="center" justify="space-between" gap="$3">
                                <XStack items="center" gap="$3" flex={1}>
                                    <Smartphone size={22} color={palette.accent} />
                                    <YStack flex={1} gap="$1">
                                        <XStack items="center" gap="$2" flexWrap="wrap">
                                            <Text numberOfLines={1}>{device.name}</Text>
                                            {device.is_current && (
                                                <Paragraph size="$1" color={palette.accent}>
                                                    This device
                                                </Paragraph>
                                            )}
                                        </XStack>
                                        <Paragraph size="$2" color={palette.secondary}>
                                            {device.platform} - last seen{" "}
                                            {formatRelativeTime(device.last_seen_at)}
                                        </Paragraph>
                                    </YStack>
                                </XStack>
                                {!device.is_current && (
                                    <Button
                                        size="$3"
                                        icon={<Trash2 size={16} color={palette.danger} />}
                                        onPress={() => void removeDevice(device)}
                                    />
                                )}
                            </XStack>
                        </ListCard>
                    ))}
                </div>
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

function ListCard({ children, onOpen }: { children: ReactNode; onOpen?: () => void }) {
    const events = onOpen ? cardEvents(onOpen) : undefined;
    return (
        <Card
            className="library-card"
            p="$3"
            bg={palette.cardFill}
            role={onOpen ? "button" : undefined}
            tabIndex={onOpen ? 0 : undefined}
            onPress={events?.onClick}
            onKeyDown={events?.onKeyDown}
        >
            {children}
        </Card>
    );
}

function EmptyState({ icon, title }: { icon: ReactNode; title: string }) {
    return (
        <YStack flex={1} items="center" justify="center" gap="$3" p="$6">
            {icon}
            <Paragraph color={palette.secondary}>{title}</Paragraph>
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
                <Paragraph maxW={620} text="center" color={palette.secondary}>
                    {message}
                </Paragraph>
            )}
        </YStack>
    );
}

function ConnectionBadge({ status }: { status: AppState["connection_status"] }) {
    const color =
        status === "Connected"
            ? palette.success
            : status === "Connecting"
              ? palette.warning
              : palette.secondary;
    return (
        <XStack items="center" gap="$2" px="$2" py="$1" rounded="$2" bg={palette.cardFill}>
            <YStack width={8} height={8} rounded={999} bg={color} />
            <Text fontSize={12} color={palette.secondary}>
                {status}
            </Text>
        </XStack>
    );
}

function isTextMimeType(mimeType: string): boolean {
    return mimeType.toLowerCase().split(";")[0]?.trim().startsWith("text/") ?? false;
}

// A collab doc's display name. Titles are optional server-side, so an untitled
// doc falls back to a short id prefix rather than rendering blank.
function collabTitle(item: { title: string; id: string }): string {
    const title = item.title.trim();
    return title.length > 0 ? title : `Untitled · ${item.id.slice(0, 8)}`;
}

// The public URL a doc's share token resolves to, or null when there is none to
// show. The server builds `share_url` from its configured `public_web_url` — the
// only place that knows where the web client is hosted, since the frontend and
// the API are separately deployed origins. A browser can fall back to its own
// origin (it *is* the web client); the Tauri shell cannot, because its origin is
// `tauri://localhost`, which is what a share link must never contain.
function shareLink(item: { share_url: string | null; share_token: string }): string | null {
    if (item.share_url) return item.share_url;
    if (isTauriRuntime()) return null;
    const origin = typeof window === "undefined" ? "" : window.location.origin;
    return origin ? `${origin}/s/${item.share_token}` : null;
}

// Shown in place of a share link when the server has no public web URL set.
const SHARE_LINK_UNAVAILABLE =
    "No share link: set the server's public web URL (CLIPPER_PUBLIC_WEB_URL).";

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
