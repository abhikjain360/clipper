import {
  palette,
  statusSurfaces,
  occurrenceId,
  occurrenceHidden,
  loadDoneMarks,
  writeDoneMark,
} from "@clipper/shared";
import {
  AlarmClock,
  Calendar,
  Clipboard,
  CookingPot,
  Copy,
  Dumbbell,
  Download,
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
  Pencil,
  RefreshCw,
  Smartphone,
  Trash2,
  X,
} from "lucide-react-native";
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from "react";
import {
  alarmsSupported,
  areNotificationsEnabled,
  cancelAllAlarms,
  canScheduleExactAlarms,
  canUseFullScreenIntent,
  dismissAlarm,
  openExactAlarmSettings,
  openFullScreenIntentSettings,
  openNotificationSettings,
  plannedAlarmCount,
  ringNow,
  setAlarms,
} from "../modules/clipper-alarm";
import {
  AppState as NativeAppState,
  Alert,
  KeyboardAvoidingView,
  Linking,
  Modal,
  PermissionsAndroid,
  Platform,
  StatusBar,
  TextInput,
  useWindowDimensions,
} from "react-native";
import { SafeAreaProvider, SafeAreaView } from "react-native-safe-area-context";
import { TamaguiProvider } from "tamagui";
import {
  Card,
  H1,
  H2,
  Label,
  Paragraph,
  ScrollView,
  Spinner,
  Switch,
  Text,
  XStack,
  YStack,
} from "tamagui";
import type {
  ActualView,
  AppState,
  CalendarSourceView,
  ClipboardItem,
  CollabItem,
  DeviceInfo,
  FileItem,
  OccurrenceView,
  RunningWorkView,
} from "@clipper/shared";
import { calendarRefreshDue, calendarSyncLabel } from "@clipper/shared";
import {
  backend,
  devDefaultServerUrl,
  formatBackendError,
  isResumeRejected,
  pickUploadFile,
  readClipboardText,
  resumeSession,
  saveCredentials,
  saveSessionConfirmation,
  shareDownloadedFile,
  writeClipboardText,
} from "./backend";
import { subscribeToCollabDoc, type CollabDocStatus } from "./collabDoc";
import { GymPanel } from "./gym/GymPanel";
import type { KitchenPlan } from "@clipper/mobile-bridge";
import { KitchenPanel } from "./kitchen/KitchenPanel";
import { armRestEndForOpenWorkout, stopRestEnd } from "./gym/restAlarm";
import tamaguiConfig, { Button, Input } from "./tamagui.config";

type TabName =
  | "clipboard"
  | "files"
  | "devices"
  | "collab"
  | "schedule"
  | "alarms"
  | "gym"
  | "kitchen";

const navItems = [
  { value: "clipboard", label: "Clipboard", Icon: Clipboard },
  { value: "files", label: "Files", Icon: Folder },
  { value: "devices", label: "Devices", Icon: Smartphone },
  { value: "collab", label: "Collab", Icon: FileCode },
  { value: "schedule", label: "Schedule", Icon: Calendar },
  { value: "alarms", label: "Alarms", Icon: AlarmClock },
  { value: "gym", label: "Gym", Icon: Dumbbell },
  { value: "kitchen", label: "Kitchen", Icon: CookingPot },
] as const satisfies readonly { value: TabName; label: string; Icon: typeof Clipboard }[];
type ViewerContent = { title: string; content: string };

// Android renders code with the platform "monospace" family; iOS has no such
// alias, so fall back to Menlo there.
const MONOSPACE_FONT = Platform.select({
  ios: "Menlo",
  android: "monospace",
  default: "monospace",
});

/**
 * How far ahead alarms are handed to the platform.
 *
 * A week rather than a day, so a device that stays offline over a weekend still
 * rings on Monday. The registry itself holds far fewer at a time — each fire
 * moves the alarm window forward from the device-protected mirror.
 */
const ALARM_WINDOW_HOURS = 24 * 7;

function clearAlarms() {
  try {
    cancelAllAlarms();
  } catch {}
  try {
    dismissAlarm();
  } catch {}
  stopRestEnd(null);
}

/** The device's IANA zone, which resolves floating alarms. */
function deviceTimeZone(): string {
  try {
    return Intl.DateTimeFormat().resolvedOptions().timeZone || "UTC";
  } catch {
    return "UTC";
  }
}

export default function App() {
  return (
    <TamaguiProvider config={tamaguiConfig} defaultTheme="dark">
      <SafeAreaProvider>
        <StatusBar barStyle="light-content" />
        <SafeAreaView style={{ flex: 1, backgroundColor: palette.pageFill }}>
          <ClipperApp />
        </SafeAreaView>
      </SafeAreaProvider>
    </TamaguiProvider>
  );
}

function ClipperApp() {
  const [state, setState] = useState<AppState | null>(null);
  const [startupError, setStartupError] = useState<string | null>(null);
  const [resumeAttempt, setResumeAttempt] = useState(0);
  const [resuming, setResuming] = useState(true);
  // Restart the state-watch loop when the session changes: a production login can
  // re-point the backend at a new server (a fresh native client), so the loop
  // must re-subscribe to that client's state channel instead of the old one's.
  const sessionKey = state?.session?.device_id ?? null;
  const currentSessionKey = useRef(sessionKey);
  currentSessionKey.current = sessionKey;
  const previousSessionKey = useRef<string | null>(null);

  useEffect(() => {
    let cancelled = false;

    async function run() {
      try {
        await backend.connect();
        await resumeSession();
      } catch (caught) {
        if (isResumeRejected(caught)) clearAlarms();
        if (!cancelled) setStartupError(formatBackendError(caught));
      } finally {
        if (!cancelled) setResuming(false);
      }
    }

    void run();

    return () => {
      cancelled = true;
    };
  }, [resumeAttempt]);

  // State-watch loop. Held until the resume attempt settles so it reads the
  // post-resume session, then re-subscribes whenever the session changes.
  useEffect(() => {
    if (resuming) return;

    let cancelled = false;
    const controller = new AbortController();

    async function run() {
      try {
        let seenVersion = await backend.stateVersion();
        if (cancelled) return;
        setState(await backend.getState());

        while (!cancelled) {
          seenVersion = await backend.waitForStateChange(seenVersion, controller.signal);
          if (!cancelled) setState(await backend.getState());
        }
      } catch (caught) {
        if (!cancelled) setStartupError(formatBackendError(caught));
      }
    }

    void run();

    return () => {
      cancelled = true;
      // Cancel the in-flight waitForStateChange UniFFI future on unmount.
      controller.abort();
    };
  }, [sessionKey, resuming]);

  const lastPushedPlan = useRef<string | null>(null);
  const [alarmRefreshGeneration, setAlarmRefreshGeneration] = useState(0);

  useEffect(() => {
    const wasAuthenticated = previousSessionKey.current !== null;
    previousSessionKey.current = sessionKey;
    if (!wasAuthenticated || sessionKey !== null) return;

    lastPushedPlan.current = null;
    clearAlarms();
  }, [sessionKey]);

  useEffect(() => {
    if (sessionKey) void saveSessionConfirmation().catch(() => {});
  }, [state, sessionKey]);

  useEffect(() => {
    const subscription = NativeAppState.addEventListener("change", (nextState) => {
      if (nextState === "active") {
        setAlarmRefreshGeneration((generation) => generation + 1);
      }
    });
    return () => subscription.remove();
  }, []);

  useEffect(() => {
    if (!alarmsSupported || !state?.session) return;

    let cancelled = false;
    const effectSessionKey = sessionKey;
    void (async () => {
      try {
        const alarms = await backend.nextAlarms?.(ALARM_WINDOW_HOURS, deviceTimeZone());
        if (cancelled || currentSessionKey.current !== effectSessionKey || !alarms) return;
        const plan = alarms.map((alarm) => ({
          itemId: alarm.item_id,
          occurrenceKey: alarm.occurrence_key,
          label: alarm.label,
          fireAtMillis: alarm.fire_at_millis,
          occurrenceStartMillis: alarm.occurrence_start_millis,
          canSnooze: alarm.can_snooze,
        }));
        const fingerprint = `${effectSessionKey}:${alarmRefreshGeneration}:${JSON.stringify(plan)}`;
        if (fingerprint === lastPushedPlan.current) return;
        setAlarms(plan);
        lastPushedPlan.current = fingerprint;
      } catch {
        // A failed push leaves the previous registry in place, which is the
        // safe outcome: stale alarms beat none. The next change retries.
      }
    })();

    return () => {
      cancelled = true;
    };
  }, [alarmRefreshGeneration, state, sessionKey]);

  if (startupError) {
    return (
      <YStack flex={1}>
        <CenteredStatus title="Cannot start Clipper" message={startupError} />
        <XStack justify="center" gap="$3" p="$4">
          <Button
            onPress={() => {
              setStartupError(null);
              setResuming(true);
              setResumeAttempt((attempt) => attempt + 1);
            }}
          >
            Retry unlock
          </Button>
          <Button
            onPress={() => {
              setStartupError(null);
              setResuming(false);
            }}
          >
            Sign in
          </Button>
        </XStack>
      </YStack>
    );
  }

  if (resuming || !state) return <CenteredStatus title="Starting Clipper" loading />;

  if (!state.session) {
    return <LoginScreen initialUsername={state.saved_profile?.username ?? ""} onState={setState} />;
  }

  return (
    <YStack flex={1}>
      <ConnectionBanner state={state} />
      <HomeScreen state={state} onState={setState} />
    </YStack>
  );
}

function ConnectionBanner({ state }: { state: AppState }) {
  const [pending, setPending] = useState(0);
  const sessionId = state.session?.device_id;
  useEffect(() => {
    let cancelled = false;
    async function refresh() {
      try {
        const status = await backend.appDataStatus?.();
        if (!cancelled) setPending(status?.pending_changes ?? 0);
      } catch {}
    }
    void refresh();
    const timer = setInterval(() => void refresh(), 1000);
    return () => {
      cancelled = true;
      clearInterval(timer);
    };
  }, [sessionId]);
  const offline = state.offline;
  if (!offline && pending === 0) return null;
  return (
    <XStack px="$3" py="$2" bg={statusSurfaces.warning}>
      <Text color={palette.warning}>
        {offline ? "Offline · " : ""}
        {pending} pending changes
      </Text>
    </XStack>
  );
}

function LoginScreen({
  initialUsername,
  onState,
}: {
  initialUsername: string;
  onState: (state: AppState) => void;
}) {
  const [mode, setMode] = useState<"login" | "register">("login");
  const [serverUrl, setServerUrl] = useState(devDefaultServerUrl());
  const [username, setUsername] = useState(initialUsername);
  const [passphrase, setPassphrase] = useState("");
  const [accessKey, setAccessKey] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const busyRef = useRef(false);

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
      // Persist behind biometric so the next cold start can resume without the
      // passphrase. Best-effort and self-contained: a cancelled/unavailable
      // biometric leaves the session unsaved without failing the login.
      await saveCredentials();
      setPassphrase("");
      setAccessKey("");
      onState(await backend.getState());
    } catch (caught) {
      setError(formatBackendError(caught));
    } finally {
      busyRef.current = false;
      setBusy(false);
    }
  }

  return (
    <KeyboardAvoidingView style={{ flex: 1 }} behavior="padding">
      <ScrollView
        flex={1}
        bg={palette.pageFill}
        contentContainerStyle={{ grow: 1 }}
        keyboardShouldPersistTaps="handled"
      >
        <YStack flex={1} items="center" justify="center" p="$4">
          <Card width="100%" maxW={460} p="$5" bg={palette.cardFill} borderWidth={0}>
            <YStack gap="$4">
              <YStack gap="$2">
                <H1 size="$9">Clipper</H1>
                <Paragraph color={palette.secondary}>Encrypted clipboard and file sync</Paragraph>
              </YStack>

              <XStack gap="$2">
                <Button flex={1} selected={mode === "login"} onPress={() => setMode("login")}>
                  Login
                </Button>
                <Button flex={1} selected={mode === "register"} onPress={() => setMode("register")}>
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
                    autoComplete="off"
                    textContentType="none"
                    importantForAutofill="no"
                    secureTextEntry
                    onChangeText={setAccessKey}
                  />
                </Field>
              )}
              <Field label="Passphrase">
                <Input value={passphrase} secureTextEntry onChangeText={setPassphrase} />
              </Field>

              {error && <Paragraph color={palette.danger}>{error}</Paragraph>}

              <Button
                tone="accent"
                disabled={busy}
                icon={busy ? <Spinner /> : undefined}
                onPress={() => void authenticate()}
              >
                {mode === "login" ? "Login" : "Register"}
              </Button>
            </YStack>
          </Card>
        </YStack>
      </ScrollView>
    </KeyboardAvoidingView>
  );
}

function HomeScreen({ state, onState }: { state: AppState; onState: (state: AppState) => void }) {
  const [tab, setTab] = useState<TabName>("clipboard");
  const [openRecipeId, setOpenRecipeId] = useState<string | null>(null);

  useEffect(() => {
    let cancelled = false;
    void armRestEndForOpenWorkout().then((open) => {
      if (open && !cancelled) setTab("gym");
    });
    return () => {
      cancelled = true;
    };
  }, []);
  const [navExpanded, setNavExpanded] = useState(false);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [runningWork, setRunningWork] = useState<RunningWorkView[] | null>(null);
  const [loggingOut, setLoggingOut] = useState(false);
  const calendarRefresh = useRef<Promise<void> | null>(null);
  const sessionId = state.session?.device_id;

  useEffect(() => {
    const subscription = NativeAppState.addEventListener("change", (nextState) => {
      if (nextState === "active")
        void backend
          .nativeClient()
          .reconnectNow()
          .catch(() => {});
    });
    return () => subscription.remove();
  }, []);

  useEffect(() => {
    let cancelled = false;
    async function refreshCalendars() {
      const previous = calendarRefresh.current;
      const task = (async () => {
        if (previous) await previous;
        try {
          const current = await backend.getState();
          if (cancelled || current.session?.device_id !== sessionId) return;
          for (const source of current.calendar_sources) {
            if (cancelled) return;
            const latest = await backend.getState();
            if (cancelled || latest.session?.device_id !== sessionId) return;
            const active = latest.calendar_sources.find((item) => item.id === source.id);
            if (!active || !calendarRefreshDue(active)) continue;
            try {
              await backend.syncCalendarSource(source.id);
            } catch (caught) {
              if (!cancelled) setError(`${active.name}: ${formatBackendError(caught)}`);
            }
          }
        } catch (caught) {
          if (!cancelled) setError(formatBackendError(caught));
        }
      })();
      calendarRefresh.current = task;
      try {
        await task;
      } finally {
        if (calendarRefresh.current === task) calendarRefresh.current = null;
      }
    }
    const timer = setTimeout(() => {
      if (NativeAppState.currentState === "active") void refreshCalendars();
    }, 2000);
    const subscription = NativeAppState.addEventListener("change", (nextState) => {
      if (nextState === "active") void refreshCalendars();
    });
    return () => {
      cancelled = true;
      clearTimeout(timer);
      subscription.remove();
    };
  }, [sessionId]);

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

  async function logout(cancelRunningWork = false) {
    setError(null);
    setLoggingOut(true);
    try {
      const outcome = await backend.logout(cancelRunningWork);
      if (outcome.status === "work_running") {
        setRunningWork(outcome.work);
        setNavExpanded(true);
        return;
      }
      setRunningWork(null);
      clearAlarms();
      onState(await backend.getState());
    } catch (caught) {
      setError(formatBackendError(caught));
    } finally {
      setLoggingOut(false);
    }
  }

  const destinations = navItems.filter((item) => item.value !== "alarms" || alarmsSupported);
  const screen = useWindowDimensions();
  const compactNavigation = screen.width < 700 || screen.height < 500;

  const navigation = (expanded: boolean) => (
    <YStack flex={1} justify="space-between" py="$2" px="$1.5" gap="$2">
      <YStack gap="$1.5">
        <Button
          chromeless
          aria-label={expanded ? "Collapse navigation" : "Expand navigation"}
          icon={expanded ? <PanelLeftClose size={20} /> : <Menu size={20} />}
          justify={expanded ? "flex-start" : "center"}
          onPress={() => setNavExpanded(!expanded)}
        />
        {destinations.map(({ value, label, Icon }) => (
          <Button
            key={value}
            aria-label={label}
            selected={tab === value}
            icon={<Icon size={20} />}
            justify={expanded ? "flex-start" : "center"}
            onPress={() => {
              setTab(value);
              setNavExpanded(false);
            }}
          >
            {expanded ? label : null}
          </Button>
        ))}
      </YStack>
      <YStack gap="$1.5">
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
        {expanded && runningWork && (
          <Card p="$3" gap="$2" borderWidth={0}>
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
        <XStack
          items="center"
          justify={expanded ? "flex-start" : "center"}
          gap="$2"
          px="$2"
          py="$2"
          aria-label={`Clipper: ${state.connection_status}`}
        >
          <Clipboard size={22} color={palette.secondary} />
          {expanded && <Text fontWeight="600">Clipper</Text>}
        </XStack>
        {expanded && <ConnectionBadge status={state.connection_status} />}
      </YStack>
    </YStack>
  );

  return (
    <XStack flex={1} bg={palette.pageFill}>
      {!compactNavigation && (
        <YStack width={64} bg={palette.cardFill} borderWidth={0}>
          {navigation(false)}
        </YStack>
      )}

      <YStack flex={1} p="$3" gap="$3">
        {compactNavigation && (
          <XStack items="center" gap="$2" mb="$-2">
            <Button
              size="$3"
              chromeless
              aria-label="Open navigation"
              icon={<Menu size={22} />}
              onPress={() => setNavExpanded(true)}
            />
            <Text color={palette.secondary}>
              {destinations.find((item) => item.value === tab)?.label ?? "Clipper"}
            </Text>
          </XStack>
        )}
        {error && <Paragraph color={palette.danger}>{error}</Paragraph>}

        {tab === "clipboard" && (
          <ClipboardPanel items={state.clipboard_items} onState={onState} onError={setError} />
        )}
        {tab === "files" && <FilesPanel files={state.files} onState={onState} onError={setError} />}
        {tab === "devices" && <DevicesPanel onError={setError} />}
        {tab === "collab" && (
          <CollabPanel
            collabDocs={state.collab_docs}
            serverUrl={state.session?.server_url ?? ""}
            onState={onState}
            onError={setError}
          />
        )}
        {tab === "schedule" && (
          <SchedulePanel
            state={state}
            onState={onState}
            onError={setError}
            onOpenRecipe={(id) => {
              setOpenRecipeId(id);
              setTab("kitchen");
            }}
          />
        )}
        {tab === "alarms" && alarmsSupported && <AlarmsPanel onError={setError} />}
        {tab === "gym" && <GymPanel onError={setError} />}
        {tab === "kitchen" && (
          <KitchenPanel
            state={state}
            openRecipeId={openRecipeId}
            onOpenRecipe={setOpenRecipeId}
            onError={setError}
          />
        )}
      </YStack>

      {navExpanded && (
        <XStack position="absolute" t={0} b={0} l={0} r={0} z={10}>
          <YStack width={220} bg={palette.cardFill} borderWidth={0}>
            {navigation(true)}
          </YStack>
          <YStack flex={1} bg="rgba(0,0,0,0.5)" onPress={() => setNavExpanded(false)} />
        </XStack>
      )}
    </XStack>
  );
}

function SchedulePanel({
  state,
  onState,
  onError,
  onOpenRecipe,
}: {
  state: AppState;
  onState: (state: AppState) => void;
  onError: (error: string | null) => void;
  onOpenRecipe: (id: string) => void;
}) {
  const [now, setNow] = useState(Date.now);
  const [occurrences, setOccurrences] = useState<OccurrenceView[]>([]);
  const [kitchenPlans, setKitchenPlans] = useState<KitchenPlan[]>([]);
  const [doneMarks, setDoneMarks] = useState<Set<string>>(new Set());
  const [showDone, setShowDone] = useState<Set<string>>(new Set());
  const [actuals, setActuals] = useState<ActualView[]>([]);
  const [loading, setLoading] = useState(true);
  const [busy, setBusy] = useState(false);
  const busyRef = useRef(false);
  const loadGeneration = useRef(0);
  const zone = deviceTimeZone();
  const today = new Date(now);
  today.setHours(0, 0, 0, 0);
  const from = today.toISOString();
  const days = useMemo(
    () =>
      Array.from({ length: 7 }, (_, index) => {
        const start = new Date(from);
        start.setDate(start.getDate() + index);
        const end = new Date(start);
        end.setDate(end.getDate() + 1);
        return { start, end };
      }),
    [from],
  );
  const weekEnd = new Date(today);
  weekEnd.setDate(weekEnd.getDate() + 7);
  const to = weekEnd.toISOString();
  const running = state.running_actual;

  const loadSchedule = useCallback(async () => {
    const generation = ++loadGeneration.current;
    setLoading(true);
    try {
      const [expanded, recorded, plans, marks] = await Promise.all([
        backend.expandSchedule(from, to, zone),
        backend.actualsBetween(from, to),
        backend
          .nativeClient()
          .kitchenPlans()
          .catch((caught: unknown) => {
            if (generation === loadGeneration.current) onError(formatBackendError(caught));
            return [];
          }),
        loadDoneMarks(backend),
      ]);
      if (generation !== loadGeneration.current) return;
      expanded.sort((a, b) => Date.parse(a.start) - Date.parse(b.start));
      recorded.sort((a, b) => Date.parse(a.start) - Date.parse(b.start));
      setOccurrences(expanded);
      setKitchenPlans(plans);
      setActuals(recorded);
      setDoneMarks(marks);
    } catch (caught) {
      if (generation === loadGeneration.current) onError(formatBackendError(caught));
    } finally {
      if (generation === loadGeneration.current) setLoading(false);
    }
  }, [from, to, zone, onError]);

  useEffect(() => {
    void loadSchedule();
    return () => {
      loadGeneration.current += 1;
    };
  }, [loadSchedule, state]);

  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), 1000);
    const subscription = NativeAppState.addEventListener("change", (nextState) => {
      if (nextState === "active") {
        setNow(Date.now());
        void loadSchedule();
      }
    });
    return () => {
      clearInterval(timer);
      subscription.remove();
    };
  }, [loadSchedule]);

  async function changeTimer(action: () => Promise<string>) {
    if (busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    onError(null);
    try {
      await action();
      setNow(Date.now());
      onState(await backend.getState());
    } catch (caught) {
      onError(formatBackendError(caught));
    } finally {
      busyRef.current = false;
      setBusy(false);
    }
  }

  async function markOccurrence(occurrence: OccurrenceView, done: boolean) {
    if (busyRef.current) return;
    busyRef.current = true;
    setBusy(true);
    onError(null);
    try {
      await writeDoneMark(backend, occurrence, done);
      setDoneMarks((current) => {
        const next = new Set(current);
        if (done) next.add(occurrenceId(occurrence));
        else next.delete(occurrenceId(occurrence));
        return next;
      });
      await loadSchedule();
    } catch (caught) {
      onError(formatBackendError(caught));
    } finally {
      busyRef.current = false;
      setBusy(false);
    }
  }

  return (
    <YStack gap="$3" flex={1} pt="$3">
      <XStack items="center" gap="$2">
        <H2 size="$6">Schedule</H2>
        {loading && <Spinner size="small" />}
      </XStack>
      <ScrollView flex={1} keyboardShouldPersistTaps="handled">
        <YStack gap="$3" pb="$4">
          <MobileCalendars
            sources={state.calendar_sources}
            now={now}
            onState={onState}
            onError={onError}
          />
          {running ? (
            <ListCard>
              <YStack gap="$2">
                <Text>{running.title || "Unplanned"}</Text>
                <Paragraph size="$2" color={palette.secondary}>
                  Started {new Date(running.start).toLocaleString(undefined, { timeZone: zone })}
                </Paragraph>
                <Text color={palette.warning}>
                  Running · {formatElapsed(now - Date.parse(running.start))}
                </Text>
                <Button
                  disabled={busy}
                  onPress={() => void changeTimer(() => backend.stopActual(running.id))}
                >
                  Stop
                </Button>
              </YStack>
            </ListCard>
          ) : (
            <Button disabled={busy} onPress={() => void changeTimer(() => backend.startActual())}>
              Start unplanned
            </Button>
          )}
          {state.schedule_warnings.map((warning, index) => (
            <Paragraph key={`${index}:${warning}`} size="$2" color={palette.warning}>
              {warning}
            </Paragraph>
          ))}
          {days.map((day, index) => {
            const planned = occurrences.filter((occurrence) =>
              overlapsScheduleDay(occurrence, day, now),
            );
            const recorded = actuals.filter((actual) => overlapsScheduleDay(actual, day, now));
            const key = day.start.toISOString();
            const hidden = planned.filter((occurrence) =>
              occurrenceHidden(occurrence, doneMarks, now),
            );
            const visible = planned.filter(
              (occurrence) => !occurrenceHidden(occurrence, doneMarks, now),
            );
            const shown = showDone.has(key) ? [...visible, ...hidden] : visible;
            return (
              <ListCard key={day.start.toISOString()}>
                <YStack gap="$3">
                  <Text fontWeight="600">
                    {index === 0 ? "Today · " : ""}
                    {day.start.toLocaleDateString(undefined, {
                      weekday: "long",
                      month: "short",
                      day: "numeric",
                      timeZone: zone,
                    })}
                  </Text>
                  {shown.length === 0 && !loading && (
                    <Paragraph size="$2" color={palette.secondary}>
                      Nothing scheduled
                    </Paragraph>
                  )}
                  {shown.map((occurrence) => (
                    <YStack
                      key={occurrenceId(occurrence)}
                      gap="$1"
                      opacity={occurrenceHidden(occurrence, doneMarks, now) ? 0.65 : 1}
                    >
                      <Text color={occurrence.cancelled ? palette.secondary : undefined}>
                        {occurrence.title}
                      </Text>
                      <Paragraph size="$2" color={palette.secondary}>
                        {scheduleTime(occurrence.start, day.start, zone)} –{" "}
                        {scheduleTime(occurrence.end, day.start, zone)}
                      </Paragraph>
                      {occurrence.all_day && (
                        <Text fontSize={12} color={palette.secondary}>
                          All day
                        </Text>
                      )}
                      {occurrence.source && (
                        <Text fontSize={12} color={palette.secondary}>
                          {occurrence.source}
                        </Text>
                      )}
                      {kitchenPlans
                        .filter(
                          (plan) =>
                            plan.itemId === occurrence.item_id &&
                            plan.occurrenceKey === occurrence.occurrence_key,
                        )
                        .map((plan, planIndex) => (
                          <Button
                            key={`${plan.recipeId}:${planIndex}`}
                            size="$3"
                            height="auto"
                            py="$2"
                            justify="flex-start"
                            icon={<CookingPot size={16} />}
                            onPress={() => onOpenRecipe(plan.recipeId)}
                          >
                            <Text color={palette.accent} shrink={1}>
                              {plan.title}
                            </Text>
                          </Button>
                        ))}
                      <XStack gap="$2" items="center">
                        {occurrence.cancelled ? (
                          <Text fontSize={12} color={palette.danger}>
                            Cancelled
                          </Text>
                        ) : (
                          <Button
                            tone="accent"
                            size="$3"
                            disabled={busy}
                            onPress={() =>
                              void changeTimer(() => backend.startActual(occurrence.plan_context))
                            }
                          >
                            Start
                          </Button>
                        )}
                        <Button
                          size="$3"
                          disabled={busy}
                          onPress={() =>
                            void markOccurrence(
                              occurrence,
                              !doneMarks.has(occurrenceId(occurrence)),
                            )
                          }
                        >
                          {doneMarks.has(occurrenceId(occurrence)) ? "Undo" : "Done"}
                        </Button>
                      </XStack>
                    </YStack>
                  ))}
                  <Button
                    size="$3"
                    aria-pressed={showDone.has(key)}
                    selected={showDone.has(key)}
                    onPress={() =>
                      setShowDone((current) => {
                        const next = new Set(current);
                        if (next.has(key)) next.delete(key);
                        else next.add(key);
                        return next;
                      })
                    }
                  >
                    Show done ({hidden.length})
                  </Button>
                  <Text fontWeight="600" color={palette.secondary}>
                    Recorded time
                  </Text>
                  {recorded.length === 0 && !loading && (
                    <Paragraph size="$2" color={palette.secondary}>
                      No recorded time
                    </Paragraph>
                  )}
                  {recorded.map((actual) => (
                    <YStack key={actual.id} gap="$1">
                      <Text>{actual.title || "Unplanned"}</Text>
                      <Paragraph size="$2" color={palette.secondary}>
                        {scheduleTime(actual.start, day.start, zone)} –{" "}
                        {actual.running ? "Running" : scheduleTime(actual.end, day.start, zone)}
                      </Paragraph>
                    </YStack>
                  ))}
                </YStack>
              </ListCard>
            );
          })}
        </YStack>
      </ScrollView>
    </YStack>
  );
}

function MobileCalendars({
  sources,
  now,
  onState,
  onError,
}: {
  sources: CalendarSourceView[];
  now: number;
  onState: (state: AppState) => void;
  onError: (error: string | null) => void;
}) {
  const [adding, setAdding] = useState(false);
  const [name, setName] = useState("");
  const [url, setUrl] = useState("");
  const [busy, setBusy] = useState<string | null>(null);
  const busyRef = useRef(false);
  const [devices, setDevices] = useState<DeviceInfo[]>([]);
  const [devicesLoading, setDevicesLoading] = useState(false);
  const [devicesError, setDevicesError] = useState<string | null>(null);
  const [ringOnSource, setRingOnSource] = useState<string | null>(null);
  const loadDevices = useCallback(async () => {
    setDevicesLoading(true);
    setDevicesError(null);
    try {
      setDevices(await backend.listDevices());
    } catch (caught) {
      setDevicesError(formatBackendError(caught));
    } finally {
      setDevicesLoading(false);
    }
  }, []);
  useEffect(() => {
    void loadDevices();
  }, [loadDevices, sources.length]);

  async function setTargetDevice(source: CalendarSourceView, targetDevice: string | null) {
    await change(source.id, async () => {
      await backend.setCalendarSourceTargetDevice(source.id, targetDevice);
      setRingOnSource(null);
    });
  }

  async function change(id: string, action: () => Promise<unknown>) {
    if (busyRef.current) return;
    busyRef.current = true;
    setBusy(id);
    onError(null);
    try {
      await action();
      onState(await backend.getState());
    } catch (caught) {
      onError(formatBackendError(caught));
    } finally {
      busyRef.current = false;
      setBusy(null);
    }
  }

  async function add() {
    await change("add", async () => {
      const id = await backend.addCalendarSource(name.trim() || "Calendar", url.trim());
      setName("");
      setUrl("");
      setAdding(false);
      onState(await backend.getState());
      await backend.syncCalendarSource(id);
    });
  }

  function remove(source: CalendarSourceView) {
    Alert.alert(
      `Remove ${source.name}?`,
      "Permanently delete this calendar's imported events and original feeds? Other calendars, recordings, local plans and local overrides stay. References to deleted imported plans will be unavailable.",
      [
        { text: "Cancel", style: "cancel" },
        {
          text: "Remove",
          style: "destructive",
          onPress: () => void change(source.id, () => backend.deleteScheduleObject(source.id)),
        },
      ],
    );
  }

  return (
    <ListCard>
      <YStack gap="$3">
        <XStack items="center" justify="space-between" gap="$2">
          <H2 size="$5">Calendars</H2>
          {!adding && (
            <Button size="$3" disabled={busy !== null} onPress={() => setAdding(true)}>
              Add calendar
            </Button>
          )}
        </XStack>
        {adding && (
          <YStack gap="$2">
            <Label htmlFor="calendar-name">Name</Label>
            <Input id="calendar-name" value={name} onChangeText={setName} placeholder="Work" />
            <Label htmlFor="calendar-url">Feed URL</Label>
            <Input
              id="calendar-url"
              value={url}
              onChangeText={setUrl}
              placeholder="https://…"
              autoCorrect={false}
              autoCapitalize="none"
              keyboardType="url"
            />
            <XStack gap="$2">
              <Button
                tone="accent"
                disabled={busy !== null || !url.trim()}
                onPress={() => void add()}
              >
                Add calendar
              </Button>
              <Button
                disabled={busy !== null}
                onPress={() => {
                  setAdding(false);
                  setUrl("");
                }}
              >
                Cancel
              </Button>
            </XStack>
          </YStack>
        )}
        {sources.length === 0 && (
          <Paragraph color={palette.secondary}>No calendars connected</Paragraph>
        )}
        {sources.map((source) => (
          <YStack key={source.id} gap="$2">
            <Text fontWeight="600">{source.name}</Text>
            <Paragraph size="$2" color={palette.secondary}>
              {source.location} · {source.event_count} events
            </Paragraph>
            <Paragraph size="$2" color={palette.secondary}>
              {calendarSyncLabel(source.checked_at, now)}
            </Paragraph>
            <Button
              size="$3"
              disabled={busy !== null}
              onPress={() => {
                setRingOnSource(ringOnSource === source.id ? null : source.id);
                void loadDevices();
              }}
            >
              Ring on:{" "}
              {source.target_device
                ? (devices.find((device) => device.id === source.target_device)?.name ??
                  "Saved device (unavailable)")
                : "All phones"}
            </Button>
            {ringOnSource === source.id && (
              <YStack gap="$2">
                <Button
                  size="$3"
                  selected={!source.target_device}
                  disabled={busy !== null}
                  onPress={() => void setTargetDevice(source, null)}
                >
                  All phones
                </Button>
                {devicesLoading ? (
                  <Spinner size="small" />
                ) : (
                  devices.map((device) => (
                    <Button
                      key={device.id}
                      size="$3"
                      selected={source.target_device === device.id}
                      disabled={busy !== null}
                      onPress={() => void setTargetDevice(source, device.id)}
                    >
                      {device.name} ({device.platform})
                    </Button>
                  ))
                )}
                {devicesError && <Paragraph color={palette.danger}>{devicesError}</Paragraph>}
                <Paragraph size="$2">
                  When feed has no reminder: {source.alarm_lead_minutes ?? 5} minutes before
                </Paragraph>
                <XStack gap="$2" flexWrap="wrap">
                  {[0, 5, 10, 15, 30].map((minutes) => (
                    <Button
                      key={minutes}
                      size="$3"
                      selected={(source.alarm_lead_minutes ?? 5) === minutes}
                      disabled={busy !== null}
                      onPress={() =>
                        void change(source.id, () =>
                          backend.setCalendarSourceAlarmLead(source.id, minutes),
                        )
                      }
                    >
                      {minutes === 0 ? "At start" : `${minutes} min before`}
                    </Button>
                  ))}
                </XStack>
                <Button size="$3" disabled={busy !== null} onPress={() => setRingOnSource(null)}>
                  Close
                </Button>
              </YStack>
            )}
            <XStack items="center" gap="$2" flexWrap="wrap">
              <Label htmlFor={`calendar-alarms-${source.id}`}>
                Alarms {source.alarms_on ? "on" : "off"}
              </Label>
              <Switch
                borderWidth={0}
                id={`calendar-alarms-${source.id}`}
                size="$3"
                checked={source.alarms_on}
                disabled={busy !== null}
                onCheckedChange={(checked) =>
                  void change(source.id, () => backend.setCalendarSourceAlarms(source.id, checked))
                }
              >
                <Switch.Thumb activeStyle={{ bg: palette.pageFill }} />
              </Switch>
              <Button
                size="$3"
                disabled={busy !== null}
                onPress={() => void change(source.id, () => backend.syncCalendarSource(source.id))}
              >
                Sync
              </Button>
              <Button size="$3" disabled={busy !== null} onPress={() => remove(source)}>
                Remove
              </Button>
              {busy === source.id && <Spinner size="small" />}
            </XStack>
          </YStack>
        ))}
      </YStack>
    </ListCard>
  );
}

function overlapsScheduleDay(
  span: { start: string; end: string },
  day: { start: Date; end: Date },
  now: number,
): boolean {
  return (
    Date.parse(span.start) < day.end.getTime() &&
    (span.end ? Date.parse(span.end) : now) > day.start.getTime()
  );
}

function scheduleTime(value: string, day: Date, zone: string): string {
  const date = new Date(value);
  const time = date.toLocaleTimeString(undefined, {
    hour: "2-digit",
    minute: "2-digit",
    timeZone: zone,
  });
  if (date.toDateString() === day.toDateString()) return time;
  return `${date.toLocaleDateString(undefined, { month: "short", day: "numeric", timeZone: zone })} ${time}`;
}

function formatElapsed(milliseconds: number): string {
  const seconds = Math.max(0, Math.floor(milliseconds / 1000));
  const hours = Math.floor(seconds / 3600);
  const minutes = Math.floor((seconds % 3600) / 60);
  return `${hours}:${String(minutes).padStart(2, "0")}:${String(seconds % 60).padStart(2, "0")}`;
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

  const [viewing, setViewing] = useState<ViewerContent | null>(null);

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

  async function viewItem(item: ClipboardItem) {
    onError(null);
    try {
      const payload = await backend.clipboardPayload(item.id);
      if (payload.text === null) {
        onError(`Cannot view ${payload.mimeType} as text`);
        return;
      }
      setViewing({ title: item.mime_type, content: payload.text });
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
        <EmptyState
          icon={<Clipboard size={28} color={palette.secondary} />}
          title="No clipboard items yet"
        />
      ) : (
        <ScrollView>
          <YStack gap="$2" pb="$4">
            {items.map((item) => (
              <ListCard key={item.id}>
                <XStack items="center" justify="space-between" gap="$3">
                  <YStack flex={1} gap="$1">
                    <Text numberOfLines={3}>{item.text}</Text>
                    <Paragraph size="$2" color={palette.secondary}>
                      {item.mime_type} - {formatRelativeTime(item.created_at)}
                    </Paragraph>
                  </YStack>
                  <XStack gap="$1">
                    <Button
                      size="$3"
                      icon={<Eye size={16} />}
                      onPress={() => void viewItem(item)}
                    />
                    <Button
                      size="$3"
                      icon={<Copy size={16} />}
                      onPress={() => void copyItem(item)}
                    />
                  </XStack>
                </XStack>
              </ListCard>
            ))}
          </YStack>
        </ScrollView>
      )}

      <ContentViewer viewing={viewing} onClose={() => setViewing(null)} onError={onError} />
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

  const [viewing, setViewing] = useState<ViewerContent | null>(null);

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

  async function viewFile(file: FileItem) {
    onError(null);
    try {
      const bytes = await backend.downloadFileBytes(file.id);
      setViewing({ title: file.filename, content: decodeUtf8(bytes) });
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
        <EmptyState icon={<Folder size={28} color={palette.secondary} />} title="No files yet" />
      ) : (
        <ScrollView>
          <YStack gap="$2" pb="$4">
            {files.map((file) => (
              <ListCard key={file.id}>
                <XStack items="center" justify="space-between" gap="$3">
                  <XStack items="center" gap="$3" flex={1}>
                    <Files size={22} color={palette.accent} />
                    <YStack flex={1} gap="$1">
                      <Text numberOfLines={1}>{file.filename}</Text>
                      <Paragraph size="$2" color={palette.secondary}>
                        {formatByteSize(file.blob_size)} - {formatRelativeTime(file.created_at)}
                      </Paragraph>
                    </YStack>
                  </XStack>
                  <XStack gap="$1">
                    {isViewableText(file.mime_type, file.filename) && (
                      <Button
                        size="$3"
                        icon={<Eye size={16} />}
                        onPress={() => void viewFile(file)}
                      />
                    )}
                    <Button
                      size="$3"
                      icon={<Download size={16} />}
                      onPress={() => void downloadFile(file)}
                    />
                    <Button
                      size="$3"
                      icon={<Trash2 size={16} color={palette.danger} />}
                      onPress={() => void deleteFile(file)}
                    />
                  </XStack>
                </XStack>
              </ListCard>
            ))}
          </YStack>
        </ScrollView>
      )}

      <ContentViewer viewing={viewing} onClose={() => setViewing(null)} onError={onError} />
    </YStack>
  );
}

/**
 * Whether alarms will actually work, and a way to prove it.
 *
 * An alarm app that silently fails is worse than no alarm app, and on Android
 * there are two ways it can: the OS may refuse exact alarms, and a vendor may
 * kill the process before one fires. The first is visible here. The second is
 * not detectable from inside the app at all — it needs the user to allow
 * autostart and exempt Clipper from battery optimisation by hand, which is why
 * that is written on the screen rather than assumed.
 */
function AlarmsPanel({ onError }: { onError: (error: string | null) => void }) {
  const [exact, setExact] = useState(true);
  const [notifications, setNotifications] = useState(true);
  const [fullScreen, setFullScreen] = useState(true);
  const [planned, setPlanned] = useState(0);

  const refresh = useCallback(() => {
    try {
      setExact(canScheduleExactAlarms());
      setNotifications(areNotificationsEnabled());
      setFullScreen(canUseFullScreenIntent());
      setPlanned(plannedAlarmCount());
    } catch (caught) {
      onError(formatBackendError(caught));
    }
  }, [onError]);

  useEffect(() => {
    refresh();
    const interval = setInterval(refresh, 2_000);
    const subscription = NativeAppState.addEventListener("change", (nextState) => {
      if (nextState === "active") refresh();
    });
    return () => {
      clearInterval(interval);
      subscription.remove();
    };
  }, [refresh]);

  const requestNotifications = useCallback(async () => {
    try {
      if (Platform.OS === "android" && Number(Platform.Version) >= 33) {
        const result = await PermissionsAndroid.request(
          PermissionsAndroid.PERMISSIONS.POST_NOTIFICATIONS,
        );
        if (
          result === PermissionsAndroid.RESULTS.NEVER_ASK_AGAIN ||
          (result === PermissionsAndroid.RESULTS.GRANTED && !areNotificationsEnabled())
        ) {
          if (!openNotificationSettings()) await Linking.openSettings();
        }
      } else if (!openNotificationSettings()) {
        await Linking.openSettings();
      }
      refresh();
    } catch (caught) {
      onError(formatBackendError(caught));
    }
  }, [onError, refresh]);

  const openFullScreenSettings = useCallback(async () => {
    try {
      if (!openFullScreenIntentSettings()) await Linking.openSettings();
    } catch (caught) {
      onError(formatBackendError(caught));
    }
  }, [onError]);

  return (
    <ScrollView flex={1}>
      <YStack gap="$3" p="$3">
        <Card bg={palette.cardFill} p="$3" gap="$2">
          <H2 size="$5">Exact alarms</H2>
          <Paragraph color={exact ? palette.success : palette.danger}>
            {exact
              ? "Exact scheduling is permitted."
              : "Exact scheduling is not permitted. Alarms will not be scheduled until you allow it."}
          </Paragraph>
          {!exact && <Button onPress={() => openExactAlarmSettings()}>Open system setting</Button>}
        </Card>

        <Card bg={palette.cardFill} p="$3" gap="$2">
          <H2 size="$5">Notifications</H2>
          <Paragraph color={notifications ? palette.success : palette.danger}>
            {notifications
              ? "Alarm notifications are enabled."
              : "Notifications are disabled. Alarms cannot show their notification."}
          </Paragraph>
          {!notifications && (
            <Button onPress={() => void requestNotifications()}>Allow notifications</Button>
          )}
        </Card>

        <Card bg={palette.cardFill} p="$3" gap="$2">
          <H2 size="$5">Full-screen alarms</H2>
          <Paragraph color={fullScreen ? palette.success : palette.danger}>
            {fullScreen
              ? "Full-screen alarm display is available."
              : "Full-screen alarm display is disabled. Android may show only a notification."}
          </Paragraph>
          {!fullScreen && (
            <Button onPress={() => void openFullScreenSettings()}>Open system setting</Button>
          )}
        </Card>

        <Card bg={palette.cardFill} p="$3" gap="$2">
          <H2 size="$5">Scheduled</H2>
          <Paragraph color={palette.secondary}>
            {planned === 1
              ? "1 upcoming alarm in the mirrored plan"
              : `${planned} upcoming alarms in the mirrored plan`}
          </Paragraph>
          <Paragraph fontSize={12} color={palette.secondary}>
            Planned alarms are mirrored to storage the system unlocks at boot, so they survive a
            restart and ring before you unlock the device. The schedule itself stays encrypted.
          </Paragraph>
          <Button onPress={refresh}>Refresh</Button>
        </Card>

        <Card bg={palette.cardFill} p="$3" gap="$2">
          <H2 size="$5">Test</H2>
          <Paragraph fontSize={12} color={palette.secondary}>
            Rings immediately, exercising the same path a real alarm takes — foreground service,
            full-screen intent, and the ring screen over the lock screen.
          </Paragraph>
          <XStack gap="$2">
            <Button tone="accent" onPress={() => ringNow("Test alarm")}>
              Ring now
            </Button>
            <Button onPress={() => dismissAlarm()}>Stop</Button>
          </XStack>
        </Card>

        <Card bg={palette.cardFill} p="$3" gap="$2">
          <H2 size="$5">Vendor settings</H2>
          <Paragraph fontSize={12} color={palette.secondary}>
            On Xiaomi, HyperOS, and similar, alarms only survive if Clipper has Autostart enabled
            and is exempt from battery optimisation. Nothing in the app can set these or detect that
            they are missing — an alarm simply never arrives.
          </Paragraph>
        </Card>
      </YStack>
    </ScrollView>
  );
}

function DevicesPanel({ onError }: { onError: (error: string | null) => void }) {
  const [devices, setDevices] = useState<DeviceInfo[] | null>(null);
  const [busy, setBusy] = useState(false);

  const loadDevices = useCallback(async () => {
    setBusy(true);
    onError(null);
    try {
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
      await backend.removeDevice(device.id);
      await loadDevices();
    } catch (caught) {
      onError(formatBackendError(caught));
    }
  }

  return (
    <YStack gap="$3" flex={1} pt="$3">
      <XStack justify="space-between" items="center" gap="$2">
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
        Removing a device signs it out everywhere and revokes its access. The objects it shared are
        kept.
      </Paragraph>

      {devices === null ? (
        <EmptyState icon={<Spinner />} title="Loading devices..." />
      ) : devices.length === 0 ? (
        <EmptyState icon={<Smartphone size={28} color={palette.secondary} />} title="No devices" />
      ) : (
        <ScrollView>
          <YStack gap="$2" pb="$4">
            {devices.map((device) => (
              <ListCard key={device.id}>
                <XStack items="center" justify="space-between" gap="$3">
                  <XStack items="center" gap="$3" flex={1}>
                    <Smartphone size={22} color={palette.accent} />
                    <YStack flex={1} gap="$1">
                      <XStack items="center" gap="$2">
                        <Text numberOfLines={1}>{device.name}</Text>
                        {device.is_current && (
                          <Paragraph size="$1" color={palette.accent}>
                            This device
                          </Paragraph>
                        )}
                      </XStack>
                      <Paragraph size="$2" color={palette.secondary}>
                        {device.platform} - last seen {formatRelativeTime(device.last_seen_at)}
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
          </YStack>
        </ScrollView>
      )}
    </YStack>
  );
}

function CollabPanel({
  collabDocs,
  serverUrl,
  onState,
  onError,
}: {
  collabDocs: CollabItem[];
  serverUrl: string;
  onState: (state: AppState) => void;
  onError: (error: string | null) => void;
}) {
  const [busy, setBusy] = useState(false);
  const [reading, setReading] = useState<CollabItem | null>(null);
  const [renaming, setRenaming] = useState<CollabItem | null>(null);

  // The open doc is re-read from the live list so a rename or a remote edit is
  // reflected in the viewer's header without closing it.
  const readingDoc = reading ? (collabDocs.find((doc) => doc.id === reading.id) ?? null) : null;

  async function createDoc() {
    setBusy(true);
    onError(null);
    try {
      await backend.createCollabDoc();
      onState(await backend.getState());
    } catch (caught) {
      onError(formatBackendError(caught));
    } finally {
      setBusy(false);
    }
  }

  async function copyLink(item: CollabItem) {
    onError(null);
    if (!item.share_url) {
      onError(SHARE_LINK_UNAVAILABLE);
      return;
    }
    try {
      await writeClipboardText(item.share_url);
    } catch (caught) {
      onError(formatBackendError(caught));
    }
  }

  async function renameDoc(item: CollabItem, title: string) {
    setRenaming(null);
    onError(null);
    try {
      await backend.renameCollabDoc(item.id, title);
      onState(await backend.getState());
    } catch (caught) {
      onError(formatBackendError(caught));
    }
  }

  async function deleteDoc(item: CollabItem) {
    onError(null);
    try {
      await backend.deleteCollabDoc(item.id);
      onState(await backend.getState());
    } catch (caught) {
      onError(formatBackendError(caught));
    }
  }

  return (
    <YStack gap="$3" flex={1} pt="$3">
      <XStack justify="space-between" items="center" gap="$2">
        <H2 size="$6">Collab Docs</H2>
        <Button
          icon={busy ? <Spinner /> : <FilePlus size={16} />}
          onPress={() => void createDoc()}
          disabled={busy}
        >
          New Doc
        </Button>
      </XStack>

      <CollabDocReader
        doc={readingDoc}
        serverUrl={serverUrl}
        onClose={() => setReading(null)}
        onError={onError}
      />
      <RenameDocDialog
        doc={renaming}
        onCancel={() => setRenaming(null)}
        onSave={(title) => renaming && void renameDoc(renaming, title)}
      />

      {collabDocs.length === 0 ? (
        <EmptyState
          icon={<FileText size={28} color={palette.secondary} />}
          title="No collab docs yet"
        />
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
                    onPress={() => setReading(item)}
                    pressStyle={{ bg: palette.cardFill }}
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
                      icon={<Pencil size={16} />}
                      onPress={() => setRenaming(item)}
                    />
                    <Button
                      size="$3"
                      icon={<Copy size={16} />}
                      onPress={() => void copyLink(item)}
                    />
                    <Button
                      size="$3"
                      icon={<Trash2 size={16} color={palette.danger} />}
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

// A collab doc's display name. Titles are optional server-side, so an untitled
// doc falls back to a short id prefix rather than rendering blank.
function collabTitle(item: { title: string; id: string }): string {
  const title = item.title.trim();
  return title.length > 0 ? title : `Untitled ${item.id.slice(0, 8)}`;
}

// Shown when the server has no public web URL configured, so it returned no
// share link. Nothing the app can fix — the link has to come from the server,
// which is the only side that knows where the web client is hosted.
const SHARE_LINK_UNAVAILABLE =
  "No share link: set the server's public web URL (CLIPPER_PUBLIC_WEB_URL).";

function Field({ label, children }: { label: string; children: ReactNode }) {
  return (
    <YStack gap="$2">
      <Label color={palette.text}>{label}</Label>
      {children}
    </YStack>
  );
}

function ListCard({ children }: { children: ReactNode }) {
  return (
    <Card p="$3" bg={palette.cardFill} borderWidth={0}>
      {children}
    </Card>
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

function EmptyState({
  icon,
  title,
  subtitle,
}: {
  icon: ReactNode;
  title: string;
  subtitle?: string;
}) {
  return (
    <YStack flex={1} items="center" justify="center" gap="$3" p="$4">
      {icon}
      <YStack items="center" gap="$1">
        <Paragraph color={palette.secondary}>{title}</Paragraph>
        {subtitle === undefined ? null : (
          <Paragraph size="$2" color={palette.secondary} style={{ textAlign: "center" }}>
            {subtitle}
          </Paragraph>
        )}
      </YStack>
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
    <YStack flex={1} items="center" justify="center" gap="$3" p="$4" bg={palette.pageFill}>
      {loading && <Spinner size="large" />}
      <H2>{title}</H2>
      {message && <Paragraph color={palette.secondary}>{message}</Paragraph>}
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

// Live read-only view of a collab doc. Editing is a desktop/web affordance (it
// needs a real code editor); on mobile the doc is worth *reading* anywhere, so
// this subscribes to the same Y-sync socket and renders whatever arrives.
function CollabDocReader({
  doc,
  serverUrl,
  onClose,
  onError,
}: {
  doc: CollabItem | null;
  serverUrl: string;
  onClose: () => void;
  onError: (error: string | null) => void;
}) {
  const [content, setContent] = useState("");
  const [status, setStatus] = useState<CollabDocStatus>("connecting");

  const docId = doc?.id;
  const shareToken = doc?.share_token;

  useEffect(() => {
    if (!docId || !shareToken) return undefined;
    if (!serverUrl) {
      onError("Not connected to a server yet.");
      return undefined;
    }

    // Reset between documents: the previous doc's text must not flash up under
    // the new one's title while the first sync is in flight.
    setContent("");
    setStatus("connecting");

    const subscription = subscribeToCollabDoc({
      objectId: docId,
      onStatus: setStatus,
      onText: setContent,
      serverUrl,
      shareToken,
    });
    return () => subscription.close();
  }, [docId, shareToken, serverUrl, onError]);

  async function copyAll() {
    try {
      await writeClipboardText(content);
    } catch (caught) {
      onError(formatBackendError(caught));
    }
  }

  return (
    <Modal visible={doc !== null} animationType="slide" onRequestClose={onClose}>
      <SafeAreaView style={{ flex: 1, backgroundColor: palette.pageFill }}>
        <XStack items="center" justify="space-between" gap="$2" px="$3" py="$2" borderWidth={0}>
          <YStack flex={1} gap="$1">
            <Text numberOfLines={1} fontWeight="600" color={palette.text}>
              {doc ? collabTitle(doc) : ""}
            </Text>
            <Paragraph size="$2" color={COLLAB_STATUS_COLORS[status]}>
              {COLLAB_STATUS_LABELS[status]}
            </Paragraph>
          </YStack>
          <Button size="$3" icon={<Copy size={16} />} onPress={() => void copyAll()} />
          <Button size="$3" icon={<X size={16} />} onPress={onClose} />
        </XStack>
        {content.length === 0 && status === "unavailable" ? (
          <EmptyState
            icon={<FileText size={28} color={palette.secondary} />}
            title="Can't open this doc"
            subtitle="It may have been deleted, or this device is offline."
          />
        ) : content.length === 0 && status !== "live" ? (
          <EmptyState icon={<Spinner />} title={COLLAB_STATUS_LABELS[status]} />
        ) : (
          <TextInput
            value={content}
            editable={false}
            multiline
            scrollEnabled
            style={{
              flex: 1,
              color: palette.text,
              backgroundColor: palette.pageFill,
              fontFamily: MONOSPACE_FONT,
              fontSize: 13,
              lineHeight: 18,
              padding: 12,
              textAlignVertical: "top",
            }}
          />
        )}
      </SafeAreaView>
    </Modal>
  );
}

const COLLAB_STATUS_LABELS: Record<CollabDocStatus, string> = {
  connecting: "Connecting…",
  live: "Live",
  offline: "Reconnecting…",
  unavailable: "Can't open this doc",
};

// `as const` keeps these literal: Tamagui's `color` prop takes a token or a
// literal colour, not an arbitrary `string`.
const COLLAB_STATUS_COLORS = {
  connecting: palette.secondary,
  live: palette.success,
  offline: palette.warning,
  unavailable: palette.danger,
} as const satisfies Record<CollabDocStatus, string>;

// Rename prompt. React Native has no `window.prompt`, and Alert.prompt is
// iOS-only, so the dialog is a small modal.
function RenameDocDialog({
  doc,
  onCancel,
  onSave,
}: {
  doc: CollabItem | null;
  onCancel: () => void;
  onSave: (title: string) => void;
}) {
  const [draft, setDraft] = useState("");

  // Seed the field each time a document is picked, not on every render, so
  // typing is not clobbered by the parent re-rendering underneath.
  useEffect(() => {
    if (doc) setDraft(doc.title);
  }, [doc]);

  return (
    <Modal visible={doc !== null} animationType="fade" transparent onRequestClose={onCancel}>
      <YStack flex={1} justify="center" p="$4" bg="rgba(0,0,0,0.6)">
        <Card p="$4" gap="$3" bg={palette.cardFill} borderWidth={0}>
          <H2 size="$5">Rename doc</H2>
          <Input
            value={draft}
            onChangeText={setDraft}
            placeholder="Untitled"
            autoFocus
            onSubmitEditing={() => onSave(draft)}
          />
          <XStack gap="$2" justify="flex-end">
            <Button size="$3" onPress={onCancel}>
              Cancel
            </Button>
            <Button size="$3" tone="accent" onPress={() => onSave(draft)}>
              Save
            </Button>
          </XStack>
        </Card>
      </YStack>
    </Modal>
  );
}

// Read-only, monospace content viewer. Files are immutable and clipboard items
// are snapshots, so there is nothing to edit here — this mirrors the web editor,
// which is also read-only. CodeMirror has no React Native build, so this is a
// plain TextInput, not a syntax-highlighting editor.
function ContentViewer({
  viewing,
  onClose,
  onError,
}: {
  viewing: ViewerContent | null;
  onClose: () => void;
  onError: (error: string | null) => void;
}) {
  async function copyAll() {
    if (!viewing) return;
    try {
      await writeClipboardText(viewing.content);
    } catch (caught) {
      onError(formatBackendError(caught));
    }
  }

  return (
    <Modal visible={viewing !== null} animationType="slide" onRequestClose={onClose}>
      <SafeAreaView style={{ flex: 1, backgroundColor: palette.pageFill }}>
        <XStack items="center" justify="space-between" gap="$2" px="$3" py="$2" borderWidth={0}>
          <Text flex={1} numberOfLines={1} fontWeight="600" color={palette.text}>
            {viewing?.title ?? ""}
          </Text>
          <Button size="$3" icon={<Copy size={16} />} onPress={() => void copyAll()} />
          <Button size="$3" icon={<X size={16} />} onPress={onClose} />
        </XStack>
        <TextInput
          value={viewing?.content ?? ""}
          editable={false}
          multiline
          scrollEnabled
          style={{
            flex: 1,
            color: palette.text,
            backgroundColor: palette.pageFill,
            fontFamily: MONOSPACE_FONT,
            fontSize: 13,
            lineHeight: 18,
            padding: 12,
            textAlignVertical: "top",
          }}
        />
      </SafeAreaView>
    </Modal>
  );
}

// Whether a file looks like UTF-8 text worth showing in the viewer. Errs toward
// showing for unknown types; genuinely binary content just renders as mojibake,
// which is acceptable for a simple viewer.
function isViewableText(mimeType: string, filename: string): boolean {
  const mime = mimeType.toLowerCase();
  if (mime.startsWith("text/")) return true;
  if (/^application\/(json|xml|javascript|x-yaml|yaml|toml|x-sh|x-shellscript)\b/.test(mime)) {
    return true;
  }
  return /\.(txt|md|markdown|json|ya?ml|toml|rs|tsx?|jsx?|py|css|html?|xml|sh|c|h|cc|cpp|hpp|go|java|kt|kts|rb|php|sql|log|ini|cfg|conf|env|lock)$/i.test(
    filename,
  );
}

// Decode downloaded file bytes as UTF-8 for the viewer. Modern Hermes ships
// TextDecoder; the byte loop is a defensive fallback if it is ever absent.
function decodeUtf8(bytes: Uint8Array): string {
  if (typeof TextDecoder !== "undefined") {
    return new TextDecoder("utf-8", { fatal: false }).decode(bytes);
  }
  let out = "";
  for (let i = 0; i < bytes.length; i += 1) out += String.fromCharCode(bytes[i] ?? 0);
  return out;
}
