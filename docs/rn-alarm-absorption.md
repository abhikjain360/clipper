# React Native + Android Alarm Absorption

Research backing [D8 in `docs/schedule-plan.md`](schedule-plan.md#d8-abnormalarm-gets-absorbed-into-clipper-rather-than-bridged-to):
whether abnormalarm's native Kotlin alarm stack can live inside Clipper's
React Native app, and what Android platform rules constrain it.

**Provenance.** Produced by a delegated research agent on 2026-09-07, then
spot-checked. Verified independently: the SDK 57 `expo prebuild` clean-by-default
change, quoted from https://expo.dev/changelog/sdk-57 as "now clears and
regenerates the native android and ios directories by default; pass `--no-clean`
to apply changes to the existing folders instead". The report's own
"claims NOT verified from a primary source" list at the end is honest and worth
reading before relying on any single item — in particular the Direct Boot
`setAlarmClock` behaviour and the `LOCKED_BOOT_COMPLETED` foreground-service
attribution question are inferences that need device testing, not documented
guarantees.

---

**Method:** Live web research following links into primary sources (developer.android.com, reactnative.dev, docs.expo.dev, support.google.com, AOSP source, GitHub issues). Every URL actually opened is listed per section and consolidated at the end. Anything not verifiable from a primary source is flagged.

---

## Executive summary (blunt)

- **Everything in your native alarm app is portable into the RN app.** You can add your exact Kotlin `BroadcastReceiver`s, the `mediaPlayback` foreground service, the full-screen ring `Activity`, and every manifest entry. Nothing about Expo prebuild or the New Architecture blocks this. The cleanest path: copy your Kotlin into the checked-in `android/` tree (or a local Expo module) and edit the checked-in `AndroidManifest.xml` directly — with one big caveat about `expo prebuild` (§1).
- **Keep the ring screen native.** Do not reimplement it in React Native. A cold RN launch takes hundreds of milliseconds to several seconds before any React UI can paint, and a JS-bundle/OTA failure means the user cannot dismiss a ringing alarm (§2).
- **Direct Boot is fully achievable in the RN app**, and your six `directBootAware` components can be ported 1:1. The JS bundle _can_ load pre-unlock in release builds (it lives in APK assets on device-encrypted storage), but you should not need JS pre-unlock — the alarm machinery should stay native. The critical rule: **anything your receivers must read before first unlock must live in device-protected storage**, via `createDeviceProtectedStorageContext()` / `moveDatabaseFrom()` / `moveSharedPreferencesFrom()` (§3).
- **One correction to a common assumption:** `setAlarmClock` **does** require an exact-alarm permission on Android 12+ (target SDK 31+). A genuine alarm-clock app should declare **`USE_EXACT_ALARM`** (auto-granted at install, not user-revocable, Play-reviewed). `setAlarmClock` remains exempt from Doze (§4).
- **Your `mediaPlayback` FGS started from the exact-alarm receiver is legal on Android 14/15/16** — exact alarms are explicitly exempt from background-FGS-start restrictions, and `mediaPlayback` has no while-in-use permission requirement. The one landmine: on Android 15+, if the alarm fires within the post-`BOOT_COMPLETED` attribution window, a `mediaPlayback` FGS start is rejected. Test reboot-immediately-before-alarm scenarios (§5).

---

## 1. Native module access in RN 0.86 + Expo 57 prebuild

### Can you add arbitrary Kotlin components? Yes.

Expo's own docs state the Expo Modules API lets you "write Swift and Kotlin to add new capabilities to your app with native modules and views," and that "Expo Modules all support the New Architecture and are automatically backwards compatible with existing React Native apps using the old architecture"
(https://docs.expo.dev/modules/overview/).

For app-local code, Expo recommends a **local module** (`npx create-expo-module@latest --local`), which scaffolds a Kotlin/Swift module under `modules/<name>/` that is "automatically linked to your app"
(https://docs.expo.dev/workflow/customizing/).

A local module's Android source set is a normal Gradle Android library — you can put `BroadcastReceiver`, `Service`, and `Activity` subclasses in it. For lifecycle hooks without editing the generated `MainActivity`/`MainApplication`, Expo provides `ReactActivityLifecycleListener` / `ApplicationLifecycleListener` (`onCreate`, `onResume`, `onPause`, `onDestroy`, `onNewIntent`, `onBackPressed`)
(https://docs.expo.dev/modules/android-lifecycle-listeners/).

**However:** since your `android/` directory is checked in and you are treating this as a bare-workflow project, you do not even need the local-module scaffolding for the alarm machinery. You can drop your Kotlin classes into `android/app/src/main/java/...` directly, exactly like a plain React Native app. The local-Expo-module route matters mainly if you want typed JS↔native bindings for the alarm layer; for pure alarm scheduling you may not need any JS bridge at all (your existing UniFFI Rust core can also be called from native code directly, which is the better pre-unlock path — see §3).

### Manifest entries: how merging actually works

Two distinct stages:

1. **Prebuild stage (Expo).** `npx expo prebuild` generates `android/app/src/main/AndroidManifest.xml` from the Expo template, then config plugins rewrite it as XML transforms (this is _not_ Android's manifest merger). The supported mechanism is `withAndroidManifest`
   (https://docs.expo.dev/modules/config-plugin-and-native-module-tutorial/). Expo's own `expo-audio` plugin is a real example of injecting a media-playback foreground service via config (`enableBackgroundPlayback` adds "a media playback foreground service" on Android)
   (https://docs.expo.dev/versions/latest/sdk/audio/).

2. **Build stage (Gradle).** Android's manifest merger then combines the main manifest with build-variant and library manifests by priority (build variant > app main > libraries), resolving conflicts with `tools:replace` / `tools:remove` / `tools:node`
   (https://developer.android.com/build/manage-manifests). Your `<service android:foregroundServiceType="mediaPlayback">`, `<receiver android:directBootAware="true">`, `<activity android:showWhenLocked="true" android:turnScreenOn="true">`, and `<uses-permission>` entries all survive this stage normally.

### Is direct editing of the checked-in `AndroidManifest.xml` viable?

**Yes — with one rule: never run `expo prebuild` against it (or understand exactly when you do).** Expo is explicit:

- "If you modify the generated directories manually then you risk losing your changes the next time you run `npx expo prebuild --clean`."
- "For existing React Native projects, where the native projects are managed manually, do not use `npx expo prebuild`, as that may overwrite any manual customizations."
  (https://docs.expo.dev/workflow/continuous-native-generation/)

**SDK 57 gotcha (important):** `expo prebuild` **now clears and regenerates `android/` and `ios/` by default**; you must pass `--no-clean` to apply changes onto existing folders
(https://expo.dev/changelog/sdk-57). So in SDK 57 an accidental bare `npx expo prebuild` (or one run by a teammate/CI script) wipes your Kotlin and manifest edits. Mitigations:

- Keep `android/` checked in and never run prebuild (treat as bare workflow). EAS Build respects this: "For a project that has android and ios directories, EAS Build will not run Prebuild to avoid overwriting any changes."
- Or move all customizations into config plugins (recommended if you want CNG to remain the source of truth).
- Middle ground: `patch-project`, which "generates and applies patches to preserve native changes after running `npx expo prebuild`" — but patches can break on SDK upgrades when templates change
  (https://docs.expo.dev/config-plugins/patch-project/).

Given you already have a complete, working native manifest with six `directBootAware` components, **direct editing of the checked-in manifest + never regenerating is the lowest-risk option**.

### New Architecture implications

None that block you. The New Architecture has been default since RN 0.76
(https://reactnative.dev/docs/the-new-architecture/landing-page), includes "an automatic interoperability layer to enable backward compatibility with libraries targeting the old architecture"
(https://reactnative.dev/blog/2024/10/23/the-new-architecture-is-here), and since RN 0.77 the old `NativeModules` object can load TurboModules
(https://reactnative.dev/blog/2025/01/21/version-0.77). Receivers, services, and activities are plain Android components — they don't touch the JS bridge at all, so TurboModule codegen is irrelevant to them.

### What could not be verified from a primary source

- No official Expo doc shows a literal config-plugin snippet adding `<service android:foregroundServiceType="mediaPlayback">` or `showWhenLocked` attributes. The capability (`withAndroidManifest` arbitrary XML edits + `expo-audio`'s documented FGS injection) makes the pattern sound, but the attribute-level snippet is inferred.

---

## 2. The ring screen: native vs React Native

**Recommendation: native Kotlin/Compose. Do not put the ring UI in React Native.** Three concrete reasons:

### (a) Cold-start latency when the process was killed

A killed-process RN launch must: init the native process → load Hermes → read and evaluate the JS bundle → mount React and commit first views. Expo's own startup-metrics guide breaks this into launch time, bundle load, time-to-render, and time-to-interactive
(https://expo.dev/guides/react-native-startup-metrics-explained). Hermes exists precisely to shorten this ("precompiled bytecode … saves the interpreter from having to perform this expensive step during app startup")
(https://reactnative.dev/blog/2022/07/08/hermes-as-the-default), but published numbers are still in the seconds range for real apps:

- Theodo (production app, low-end Android): 12.9 s → 3.9 s cold start _after_ adopting Hermes
  (https://apps.theodo.com/en/radar-2023/react-native).
- RapidNative 2026 playbook: 3.8 s cold start on a mid-tier Pixel-6a-class device
  (https://www.rapidnative.com/blogs/react-native-performance-optimization-2026-playbook).
- RN 0.82 / Hermes V1 improved bundle load only a few percent on low-end Android
  (https://reactnative.dev/blog/2025/10/08/react-native-0.82).

A native Activity paints its first frame in tens of milliseconds. For an alarm that must be visible and dismissible on the first ring, a multi-second blank/splash window is disqualifying on its own.

### (b) Reliability if the JS bundle fails to load

Bundle-load failure is a well-documented failure mode — white screen, crash, or unresponsive activity:

- Classic release-build failure: "Unable to load script … bundle `index.android.bundle` … packaged correctly for release."
- OTA (CodePush) failure: "Failed to load bundle … main.jsbundle"
  (https://github.com/Microsoft/react-native-code-push/issues/1197).
- `expo-updates` error recovery is explicitly "not a full safety net … in many cases, users will still see a crash," and fatal errors thrown >10 s after first render are not caught at all
  (https://docs.expo.dev/eas-update/error-recovery/).
- Real Expo SDK 54 report: blank screen with an empty Android Activity that also blocked live updates
  (https://github.com/expo/expo/issues/41543); `Updates.reloadAsync()` freeze requiring a `ReactRootView` detach/reattach patch
  (https://andrei-calazans.com/posts/expo-updates-stuck-on-android-when-force-update/).

If the bundle fails at alarm time, the user has no dismiss button while audio blares. A native ring screen has no such dependency.

### (c) Launching from a BroadcastReceiver while dozing/locked

- Since Android 10, "the platform has placed restrictions on when apps can start activities from the background"
  (https://developer.android.com/guide/components/activities/background-starts). Directly calling `startActivity()` from an alarm receiver is unreliable. The platform-sanctioned path is a high-priority notification with a **full-screen intent**, which requires `USE_FULL_SCREEN_INTENT` (normal permission, auto-granted)
  (https://developer.android.com/about/versions/10/behavior-changes-10#background-activity-starts).
- Android 14 narrowed this: default FSI grant is "limited to those that provide **calling and alarms only**. The Google Play Store revokes default `USE_FULL_SCREEN_INTENT` permissions for any apps that don't fit this profile," and apps get `NotificationManager.canUseFullScreenIntent()` + `ACTION_MANAGE_APP_USE_FULL_SCREEN_INTENT` to check/request
  (https://developer.android.com/about/versions/14/behavior-changes-14). A genuine alarm app qualifies — but note this policy now applies to the _merged_ RN app as a whole.
- The FSI window **can** appear over the lock screen before unlock (a Notifee user reported it bypassing the lockscreen entirely: https://github.com/invertase/notifee/issues/501). But "window appears" ≠ "React UI rendered" — with a cold process the user stares at a window background/splash/blank screen while Hermes boots. OEM behavior is additionally hostile: full-screen notifications degrade to an icon on Samsung One UI always-on display
  (https://github.com/invertase/notifee/issues/584), and foreground-notification misconfiguration silently drops the FSI
  (https://github.com/invertase/notifee/issues/317).

### (d) Real-world attempts

- `baekgol/react-native-alarm-manager`: bare-workflow RN alarm lib requiring manifest edits, `MainActivity` overrides, bundled raw sounds — i.e., the ring path is native anyway
  (https://github.com/baekgol/react-native-alarm-manager).
- `joaoGabriel55/react-native-alarmageddon`: documents needing `SCHEDULE_EXACT_ALARM`, warns "Some device manufacturers (Samsung, Xiaomi, Huawei, etc.) may kill background processes," and is explicitly "not compatible with Expo Go"
  (https://github.com/joaoGabriel55/react-native-alarmageddon).
- `Alperengozum/expo-alarm` only wraps the _system_ clock app's `ACTION_SET_ALARM` — you don't own the ring UI at all
  (https://github.com/Alperengozum/expo-alarm).
- Community consensus (Stack Overflow "Should I build an alarm app in React Native or natively"): native for the alarm path
  (https://stackoverflow.com/questions/73909084/should-i-build-an-alarm-app-in-react-native-or-build-it-natively-for-ios-and-and).

**Bottom line:** everyone who ships a serious alarm app ends up implementing the ring screen natively, even inside RN apps. You already _have_ that native implementation — port it unchanged. Use RN for the alarm list, editing, and settings screens, where a 1–3 s cold start is acceptable.

**Not verified from primary sources:** Meta publishes no official cold-start millisecond numbers for killed-process RN launches (the figures above are community/consulting benchmarks). The exact Play Console `USE_FULL_SCREEN_INTENT` declaration-form text was not retrievable; the Android 14 behavior-changes page paraphrases the policy.

---

## 3. Direct Boot (the critical question)

### What Direct Boot is

On file-based-encryption devices (Android 7+), after power-on but **before the user's first unlock**, the OS is running but app data is split into two tiers:

- **Device-encrypted / device-protected (DE) storage** — available immediately at boot. On disk: `/data/user_de/<userId>/<package>/`.
- **Credential-encrypted / credential-protected (CE) storage** — the **default**; unlocked only after the user enters their credentials. On disk: `/data/user/<userId>/<package>/`.

(Directory constants `DIR_USER_CE = "user"` and `DIR_USER_DE = "user_de"` from AOSP `Environment.java`; `LoadedApk` sets the default `mDataDirFile` to the credential-protected dir — links below.)

Broadcast order and storage availability:

| Broadcast                      | When                       | Storage available    | Requirement                               |
| ------------------------------ | -------------------------- | -------------------- | ----------------------------------------- |
| `ACTION_LOCKED_BOOT_COMPLETED` | User still locked          | DE only              | Receiver must be `directBootAware="true"` |
| `ACTION_USER_UNLOCKED`         | User unlocks               | CE becomes available | —                                         |
| `ACTION_BOOT_COMPLETED`        | After unlock/boot finished | DE + CE              | `RECEIVE_BOOT_COMPLETED` permission       |

AOSP `Intent.java`: "Upon receipt of this broadcast, the user is still locked and only device-protected storage can be accessed safely … To receive this broadcast, your receiver component must be marked as being `ComponentInfo.directBootAware`."

### What `android:directBootAware="true"` actually guarantees

Per https://developer.android.com/privacy-and-security/direct-boot: "To mark your component as encryption aware, set the `android:directBootAware` attribute to true … Encryption aware components can register to receive an `ACTION_LOCKED_BOOT_COMPLETED` broadcast … such as triggering a scheduled alarm."

**It guarantees only this: the component may run pre-unlock and may access DE storage.** It does _not_ relocate anything; the default `Context` still points at CE storage, which remains inaccessible. If the component touches default SharedPreferences, default Room/SQLite, or `filesDir` pre-unlock, it fails.

### (a) Can a React Native JS bundle load in direct boot mode?

**Yes, in a standard release build** — and the reason is specific. The release JS bundle (`index.android.bundle`) is packaged in the APK under `assets/` and loaded via `context.getAssets()` (`JSBundleLoader.createAssetLoader`, used by default in `ReactInstanceManagerBuilder` — RN source links below). The APK itself sits under `/data/app/`, which is on device-encrypted storage and readable pre-unlock. So a `directBootAware` activity _could_ boot Hermes and load the bundle before first unlock.

**But you should not rely on this, and mostly don't need to:**

- Any JS touching CE-backed APIs (default `AsyncStorage`, default SQLite/Room, default SharedPreferences, `filesDir`) fails until unlock.
- If you ever ship the bundle via OTA (`expo-updates` writes it under `filesDir`, i.e. CE storage), it becomes unreadable pre-unlock. This alone is a reason the pre-unlock alarm path must not depend on JS.
- SoLoader's extraction path defaults to `dataDir` (CE); modern builds use `android:extractNativeLibs="false"` and load `.so`s from the APK's `lib/` dir, which is fine.

**Practical architecture:** your alarm scheduling, alarm-state storage, receivers, FGS, and ring `Activity` stay native and DE-aware; they call your Rust core directly from Kotlin (UniFFI bindings are callable from native code, no JS involved — note the Rust core's own storage must likewise be DE-aware if read pre-unlock). React Native never runs before first unlock. The RN layer picks up alarm state after unlock via `BOOT_COMPLETED`/`ACTION_USER_UNLOCKED` or normal app start.

### (b) Room DB in default (CE) storage + `directBootAware` receiver on `LOCKED_BOOT_COMPLETED`

The receiver **runs**, but it **cannot read its alarms**. CE storage is "only available after the user has unlocked the device" (Direct Boot guide). For SharedPreferences the platform throws explicitly (AOSP `ContextImpl.java`):

> `IllegalStateException: SharedPreferences in credential encrypted storage are not available until after user (id 0) is unlocked`

For Room/SQLite the docs name no specific exception; empirically it surfaces as `SQLiteCantOpenDatabaseException` / `SQLITE_CANTOPEN`. **I could not verify the exact SQLite exception text from a primary Android source** — the documented, safe conclusion is simply: CE files are inaccessible, the open fails, and your receiver sees zero alarms. If your receiver swallows that failure, the net effect is a **missed alarm after every reboot** — the worst possible alarm-app bug.

### (c) The standard pattern real alarm apps use

1. Store the alarm schedule (times, enabled flags, ringtone, snooze config) in **device-protected storage**.
2. Register a `directBootAware` receiver for `LOCKED_BOOT_COMPLETED` (fall back to `BOOT_COMPLETED` pre-N) that re-schedules all alarms with `AlarmManager`, because **all alarms are cleared on shutdown**.
3. Mark the alarm-trigger receiver, the FGS, and the ring activity `directBootAware` so the whole fire path works pre-unlock.

This is exactly what AOSP DeskClock (the stock Android clock) does:

- Manifest: `AlarmInitReceiver` with `android:directBootAware="true"` and both `BOOT_COMPLETED` and `LOCKED_BOOT_COMPLETED` filters.
- `AlarmInitReceiver.kt`: uses `Intent.ACTION_LOCKED_BOOT_COMPLETED` on N+, `BOOT_COMPLETED` otherwise.
- `DeskClockApplication.kt` migrates preferences into DE storage:

```kotlin
val name = PreferenceManager.getDefaultSharedPreferencesName(context)
storageContext = context.createDeviceProtectedStorageContext()
if (!storageContext.moveSharedPreferencesFrom(context, name)) { ... }
return PreferenceManager.getDefaultSharedPreferences(storageContext)
```

(AOSP DeskClock manifest / `AlarmInitReceiver.kt` / `DeskClockApplication.kt` — links below. The AOSP-derived BlackyHawky/Clock does the identical migration. Interestingly, Fossify Clock and yuriykulikov/Simple-Alarm-Clock only listen for `BOOT_COMPLETED` and do **not** implement Direct Boot — meaning on FBE devices their alarms silently don't reschedule until first unlock. Don't copy them.)

For a Room database, the equivalent is either building Room with a DE context for the alarm-schedule DB:

```kotlin
val de = context.createDeviceProtectedStorageContext()
val db = Room.databaseBuilder(de, AlarmDb::class.java, "alarms.db").build()
```

or migrating an existing CE database with `Context.moveDatabaseFrom(sourceContext, name)` on upgrade (documented on the Direct Boot guide alongside `moveSharedPreferencesFrom`). A pragmatic split used by real apps: keep the minimal pre-unlock schedule in DE storage (or a small DE Room DB) and the rest of the app's data in CE.

### Does `setAlarmClock` fire while the device is still locked (direct boot mode)?

There is **no primary-source sentence stating this explicitly** — flagged as inferred. But: `AlarmManager` lives in `system_server` and is not gated on CE storage; the Direct Boot guide names alarm clocks as the canonical direct-boot use case ("triggering a scheduled alarm" on `LOCKED_BOOT_COMPLETED`); and the `AlarmManager` reference says `setAlarmClock` alarms "will be allowed to trigger even if the system is in a low-power idle (a.k.a. doze) mode." The delivery requirement is that the `PendingIntent` target (receiver/service/activity) be `directBootAware`, otherwise delivery is deferred until unlock. So: yes in practice, provided the whole fire path is `directBootAware` and its data is in DE storage.

---

## 4. Exact alarms in 2026 (Android 14/15/16)

### `USE_EXACT_ALARM` vs `SCHEDULE_EXACT_ALARM`

From https://developer.android.com/develop/background-work/services/alarms/schedule and the Android 14 changes page:

|                | `USE_EXACT_ALARM`                                                                                                  | `SCHEDULE_EXACT_ALARM`                                                                                                                           |
| -------------- | ------------------------------------------------------------------------------------------------------------------ | ------------------------------------------------------------------------------------------------------------------------------------------------ |
| Grant          | Automatic at install                                                                                               | **Denied by default** on Android 14+ for fresh installs targeting API 33+                                                                        |
| User-revocable | No                                                                                                                 | Yes (user and system; revocation **deletes** all scheduled exact alarms)                                                                         |
| Play policy    | Restricted — review required; acceptable use cases are alarm/timer apps and calendar apps with event notifications | Treated as the general fallback; no equivalent Play restriction section found                                                                    |
| Runtime flow   | None needed                                                                                                        | `canScheduleExactAlarms()` → if false, `ACTION_REQUEST_SCHEDULE_EXACT_ALARM` → listen for `ACTION_SCHEDULE_EXACT_ALARM_PERMISSION_STATE_CHANGED` |

Play policy (support.google.com answer 13161072): "`USE_EXACT_ALARM` … is a highly restricted permission used only for apps whose core, user-facing functionality genuinely requires precise timing, like dedicated alarm, timer, or calendar applications … Apps that request this restricted permission are subject to review." Acceptable use cases listed: alarm/timer apps, calendar apps.

**Recommendation: declare `USE_EXACT_ALARM`.** Your app is a genuine alarm-clock app — the exact category Play carves out. It is granted on install and cannot be silently revoked, which eliminates the entire "user denied Alarms & reminders, alarm silently deleted" failure class. Keep `canScheduleExactAlarms()` checks as a defensive measure anyway. Caveat: the merged RN app must pass Play review as an alarm-clock app; if alarms become a minor side feature of a clipboard/productivity app, `USE_EXACT_ALARM` may be rejected in review and you'd fall back to `SCHEDULE_EXACT_ALARM` with its user-grant flow.

### `setAlarmClock`: permission requirement and Doze

**Correction to the common assumption:** per the `AlarmManager` reference (https://developer.android.com/reference/android/app/AlarmManager):

- "Starting with `Build.VERSION_CODES.S`, apps targeting SDK level 31 or higher need to request the `SCHEDULE_EXACT_ALARM` permission to use this API." So `setAlarmClock` **is** gated on the exact-alarm permission model (satisfied by `USE_EXACT_ALARM` too). The only documented exception from the permission check is `setExact` with an `OnAlarmListener` — it does **not** extend to `setAlarmClock`.
- Revocation consequence: "When the user revokes the `SCHEDULE_EXACT_ALARM` permission, all alarms scheduled with `setExact(...)`, `setExactAndAllowWhileIdle(...)` and `setAlarmClock(...)` will be deleted."
- Doze: "these alarms will be allowed to trigger even if the system is in a low-power idle (a.k.a. doze) mode" — **still exempt from Doze**, and "Alarms scheduled via this API will be allowed to start a foreground service even if the app is in the background" (see §5).

### What changed in Android 15 / 16

**Nothing new for exact alarms.** I checked both the "all apps" and "apps targeting X" behavior-changes pages for Android 15 and 16: no new exact-alarm or "Alarms & reminders" changes are documented. The Android 14 default-denial regime is the current state. Alarm quotas by app-standby bucket (working set: 10/hour; frequent: 2/hour; rare: 1/hour; restricted: 1/day) still apply on https://developer.android.com/topic/performance/power/power-details — `setAlarmClock` alarms by an active alarm app are not practically affected, but quota behavior is worth knowing. The only Android 16 resource-limit change is to JobScheduler quotas, not alarms.

**Android 16 Live Updates / promoted ongoing notifications** (your existing app uses these): this is a notification feature, unrelated to the alarm permission model. Requirements per https://developer.android.com/develop/ui/views/notifications/live-update: a standard style (`ProgressStyle` etc.), the non-runtime `android.permission.POST_PROMOTED_NOTIFICATIONS` permission, `EXTRA_REQUEST_PROMOTED_ONGOING` / `setRequestPromotedOngoing`, `ongoing`, and "must not be a reminder or upcoming calendar event." **I found no primary source connecting Live Updates to alarm APIs** — your current promoted-ongoing usage remains a notification-layer feature and ports unchanged.

**Could not verify:** a separate Play policy restricting `SCHEDULE_EXACT_ALARM` (the Play page only reviews `USE_EXACT_ALARM`); any Android 15/16 exact-alarm behavior change (absence of evidence in the official pages, not proof of absence).

---

## 5. Foreground service types on Android 14+

### Starting a `mediaPlayback` FGS from an exact-alarm receiver: required declarations

All four you already have, plus the exact-alarm permission:

```xml
<uses-permission android:name="android.permission.FOREGROUND_SERVICE" />
<uses-permission android:name="android.permission.FOREGROUND_SERVICE_MEDIA_PLAYBACK" />
<service android:name=".RingService"
         android:foregroundServiceType="mediaPlayback" ... />
```

and at runtime `startForeground(id, notification, ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PLAYBACK)`. Missing the manifest type → `MissingForegroundServiceTypeException`; passing an undeclared type to `startForeground()` → `IllegalArgumentException` (https://developer.android.com/develop/background-work/services/fgs/declare, .../fgs/launch).

### Is it allowed from the background? Yes — exact alarms are exempt

The key quote (https://developer.android.com/develop/background-work/services/alarms):

> "Android considers exact alarms to be critical, time-sensitive interruptions. For this reason, **exact alarms aren't affected by foreground service launch restrictions**."

And the FGS restrictions page lists as an exemption: "Your app invokes an **exact alarm** to complete an action that the user requests"
(https://developer.android.com/develop/background-work/services/fgs/restrictions-bg-start).

`mediaPlayback` specifically has **no while-in-use runtime permission prerequisite** (its row in the FGS-types table lists "Runtime prerequisites: None"), unlike `camera`/`microphone`/`location`, which throw `SecurityException` when started from the background without while-in-use grants
(https://developer.android.com/develop/background-work/services/fg-service-types). So: exact-alarm exemption + mediaPlayback's no-prerequisite status = your ring service can legally start from a dozing, background, or even locked device.

Note: **full-screen intent is not an FGS-start exemption** (it's an activity/notification mechanism), and neither is "user turned screen on." The alarm exemption is what you're relying on.

### The Android 15 landmine: BOOT_COMPLETED attribution

Android 15 forbids launching these FGS types from a `BOOT_COMPLETED` receiver: `dataSync`, `camera`, **`mediaPlayback`**, `phoneCall`, `mediaProjection`, `microphone`
(https://developer.android.com/about/versions/15/behavior-changes-15, confirmed per-type on https://developer.android.com/about/versions/15/changes/foreground-service-types).

Crucially, the restriction follows **attribution, not the immediate caller**. A real-world report (https://github.com/gdelataillade/alarm/issues/424) documents that if an alarm fires inside the temporary post-`BOOT_COMPLETED` allowlist window (roughly the first ~45 s after boot), the FGS start is still attributed to `BOOT_COMPLETED` and `mediaPlayback` is **rejected**; outside that window the same delivery is attributed to `ALARM_MANAGER_WHILE_IDLE` and succeeds.

**Consequence for you:** an alarm scheduled to ring in the first minute after a reboot can fail to start the ring service on Android 15/16. Mitigations to test: use `LOCKED_BOOT_COMPLETED` instead of `BOOT_COMPLETED` for rescheduling (different attribution — and required anyway for direct boot, §3), and wrap the FGS start with a retry/`ForegroundServiceStartNotAllowedException` fallback that rings via the notification's full-screen intent + sound. **The `LOCKED_BOOT_COMPLETED` → FGS attribution path is not explicitly documented** — flagged as needing device testing.

### Timeouts and Android 16

- `dataSync` and `mediaProcessing` FGS get a 6 h / 24 h cumulative timeout with `Service.onTimeout(int, int)` on Android 15+ (https://developer.android.com/develop/background-work/services/fgs/timeout). `shortService` has ~3 minutes. **`mediaPlayback` has no such timeout** — fine for an alarm ring, though you should still self-stop on snooze/dismiss.
- Android 16 FGS changes: jobs started from an FGS must now follow job quotas (https://developer.android.com/develop/background-work/services/fgs/changes), and `FOREGROUND_SERVICE_TYPE_HEALTH` gained new permission granularity. **No `mediaPlayback`-specific Android 16 restriction is documented** — third-party blogs claim tighter background-audio rules, but I could not verify this from any official source. Flagged as unverified.

---

## Recommended absorption plan (from the findings)

1. Copy your Kotlin receivers, ring service, ring activity, torch/vibration code, and Compose ring UI into the checked-in `android/` tree unchanged. Port the manifest entries (including all six `directBootAware` components) into `android/app/src/main/AndroidManifest.xml` directly.
2. Institute a team rule: **never run `npx expo prebuild`** (SDK 57 defaults to `--clean` and will wipe everything). If you want to keep CNG, invest in config plugins instead — but with this much native surface, direct editing is simpler.
3. Keep the RN surface to alarm list/editing/settings. Bridge alarm state to JS via a small Expo local module or TurboModule, or read shared state through your UniFFI Rust core.
4. Split storage by boot tier: minimal pre-unlock alarm schedule in device-protected storage (`createDeviceProtectedStorageContext`, `moveDatabaseFrom`/`moveSharedPreferencesFrom` for existing installs); everything else stays in CE. If the Rust core is read pre-unlock, its storage path must be DE-aware too.
5. Declare `USE_EXACT_ALARM` (alarm clock is a Play-accepted use case), keep `SCHEDULE_EXACT_ALARM` handling as fallback, and keep the `canScheduleExactAlarms()` check.
6. Add device tests for: reboot → alarm rings pre-unlock (direct boot path); reboot → alarm fires within ~45 s of boot (the Android 15 `BOOT_COMPLETED` attribution trap); Samsung One UI FSI degradation; OTA-update corruption not affecting the (native) ring path.

---

## All URLs actually opened during this research

**developer.android.com**

- https://developer.android.com/privacy-and-security/direct-boot
- https://developer.android.com/training/articles/direct-boot
- https://developer.android.com/reference/android/app/AlarmManager
- https://developer.android.com/build/manage-manifests
- https://developer.android.com/guide/components/activities/background-starts
- https://developer.android.com/about/versions/10/behavior-changes-10#background-activity-starts
- https://developer.android.com/about/versions/14/behavior-changes-14
- https://developer.android.com/about/versions/14/changes/schedule-exact-alarms
- https://developer.android.com/about/versions/15/behavior-changes-15
- https://developer.android.com/about/versions/15/behavior-changes-all
- https://developer.android.com/about/versions/15/features
- https://developer.android.com/about/versions/15/changes/foreground-service-types
- https://developer.android.com/about/versions/16/behavior-changes-16
- https://developer.android.com/about/versions/16/behavior-changes-all
- https://developer.android.com/develop/background-work/services/alarms
- https://developer.android.com/develop/background-work/services/alarms/schedule
- https://developer.android.com/develop/background-work/services/fg-service-types
- https://developer.android.com/develop/background-work/services/fgs/restrictions-bg-start
- https://developer.android.com/develop/background-work/services/fgs/changes
- https://developer.android.com/develop/background-work/services/fgs/declare
- https://developer.android.com/develop/background-work/services/fgs/launch
- https://developer.android.com/develop/background-work/services/fgs/timeout
- https://developer.android.com/develop/ui/views/notifications/time-sensitive
- https://developer.android.com/develop/ui/views/notifications/live-update
- https://developer.android.com/develop/ui/views/notifications/build-notification
- https://developer.android.com/develop/ui/views/notifications/notification-permission
- https://developer.android.com/topic/performance/power/power-details

**AOSP source (android.googlesource.com)**

- https://android.googlesource.com/platform/frameworks/base/+/master/core/java/android/content/Intent.java?format=TEXT
- https://android.googlesource.com/platform/frameworks/base/+/master/core/java/android/os/Environment.java?format=TEXT
- https://android.googlesource.com/platform/frameworks/base/+/master/core/java/android/app/ContextImpl.java?format=TEXT
- https://android.googlesource.com/platform/frameworks/base/+/master/core/java/android/app/LoadedApk.java?format=TEXT
- https://android.googlesource.com/platform/frameworks/base/+/master/core/java/android/content/Context.java?format=TEXT
- https://android.googlesource.com/platform/frameworks/base/+/master/core/java/android/content/pm/ApplicationInfo.java?format=TEXT
- https://android.googlesource.com/platform/packages/apps/DeskClock/+/master/AndroidManifest.xml?format=TEXT
- https://android.googlesource.com/platform/packages/apps/DeskClock/+/master/src/com/android/deskclock/AlarmInitReceiver.kt?format=TEXT
- https://android.googlesource.com/platform/packages/apps/DeskClock/+/master/src/com/android/deskclock/DeskClockApplication.kt?format=TEXT
- https://android.googlesource.com/platform/packages/apps/DeskClock/+/master/src/com/android/deskclock/data/DataModel.kt?format=TEXT

**reactnative.dev**

- https://reactnative.dev/docs/the-new-architecture/landing-page
- https://reactnative.dev/blog/2024/10/23/the-new-architecture-is-here
- https://reactnative.dev/blog/2025/01/21/version-0.77
- https://reactnative.dev/blog/2022/07/08/hermes-as-the-default
- https://reactnative.dev/blog/2025/10/08/react-native-0.82
- https://reactnative.dev/blog/2026/06/11/react-native-0.86
- https://reactnative.dev/docs/turbo-native-modules-introduction

**docs.expo.dev / expo.dev**

- https://docs.expo.dev/workflow/continuous-native-generation/
- https://docs.expo.dev/workflow/customizing/
- https://docs.expo.dev/modules/config-plugin-and-native-module-tutorial/
- https://docs.expo.dev/config-plugins/development-and-debugging/
- https://docs.expo.dev/guides/adopting-prebuild/
- https://docs.expo.dev/config-plugins/patch-project/
- https://docs.expo.dev/modules/overview/
- https://docs.expo.dev/modules/get-started/
- https://docs.expo.dev/modules/native-module-tutorial/
- https://docs.expo.dev/modules/android-lifecycle-listeners/
- https://docs.expo.dev/brownfield/lifecycle-listeners/
- https://docs.expo.dev/versions/latest/sdk/audio/
- https://docs.expo.dev/versions/latest/config/app/
- https://docs.expo.dev/eas-update/error-recovery/
- https://expo.dev/changelog/sdk-57
- https://expo.dev/guides/react-native-startup-metrics-explained

**GitHub (issues, repos, source)**

- https://github.com/expo/expo/issues/42754
- https://github.com/expo/expo/issues/41543
- https://github.com/expo/expo/issues/14930
- https://github.com/expo/expo/issues/11652
- https://github.com/expo/expo/issues/41627
- https://github.com/invertase/notifee/issues/501
- https://github.com/invertase/notifee/issues/317
- https://github.com/invertase/notifee/issues/584
- https://github.com/gdelataillade/alarm/issues/424
- https://github.com/baekgol/react-native-alarm-manager
- https://github.com/Alperengozum/expo-alarm
- https://github.com/joaoGabriel55/react-native-alarmageddon
- https://github.com/Microsoft/react-native-code-push/issues/1197
- https://github.com/react-native-community/upgrade-support/issues/265
- https://raw.githubusercontent.com/react/react-native/main/packages/react-native/ReactAndroid/src/main/java/com/facebook/react/bridge/JSBundleLoader.kt
- https://raw.githubusercontent.com/react/react-native/main/packages/react-native/ReactAndroid/src/main/java/com/facebook/react/ReactInstanceManagerBuilder.kt
- https://raw.githubusercontent.com/facebook/SoLoader/main/java/com/facebook/soloader/UnpackingSoSource.java
- https://raw.githubusercontent.com/BlackyHawky/Clock/main/app/src/main/AndroidManifest.xml
- https://raw.githubusercontent.com/BlackyHawky/Clock/main/app/src/main/java/com/best/deskclock/alarms/AlarmInitReceiver.java
- https://raw.githubusercontent.com/BlackyHawky/Clock/main/app/src/main/java/com/best/deskclock/DeskClockApplication.java
- https://raw.githubusercontent.com/FossifyOrg/Clock/main/app/src/main/AndroidManifest.xml
- https://raw.githubusercontent.com/yuriykulikov/AlarmClock/develop/app/src/main/AndroidManifest.xml

**Google blogs / Play policy**

- https://android-developers.googleblog.com/2016/04/developing-for-direct-boot.html
- https://android-developers.googleblog.com/2025/06/android-16-is-here.html
- https://android-developers.googleblog.com/2025/05/16-things-to-know-for-android-developers-google-io-2025.html
- https://support.google.com/googleplay/android-developer/answer/13161072

**Third-party / community**

- https://notifee.app/react-native/docs/android/behaviour/
- https://notifee.app/react-native/reference/notificationfullscreenaction/
- https://www.callstack.com/blog/optimize-android-app-startup-time-with-hermes
- https://www.callstack.com/blog/hermes-performance-on-ios
- https://apps.theodo.com/en/radar-2023/react-native
- https://www.rapidnative.com/blogs/react-native-performance-optimization-2026-playbook
- https://proandroiddev.com/full-screen-intent-fsi-notifications-in-android-14-15-what-changed-why-its-breaking-and-e5e862a75936
- https://stackoverflow.com/questions/73909084/should-i-build-an-alarm-app-in-react-native-or-build-it-natively-for-ios-and-and
- https://stackoverflow.com/questions/57868564/full-screen-intent-not-starting-the-activity-but-do-show-a-notification-on-andro
- https://proxyman.com/posts/2024-09-16-unable-to-load-script-with-react-native-android-with-metro-bundler%20copy
- https://andrei-calazans.com/posts/expo-updates-stuck-on-android-when-force-update/
- https://www.volcengine.com/article/2543102

_(Attempted URLs that returned 404 — e.g. developer.android.com/about/versions/16/changes/foreground-service-types — are omitted.)_

---

## Consolidated list of claims NOT verified from a primary source

1. Exact attribute-level Expo config-plugin snippet for `<service android:foregroundServiceType="mediaPlayback">` / `showWhenLocked` (capability documented; snippet inferred).
2. Official Meta/reactnative.dev cold-start millisecond numbers for killed-process RN launches (only community benchmarks exist).
3. Exact Play Console `USE_FULL_SCREEN_INTENT` declaration-form wording.
4. Exact SQLite exception class when opening a CE Room database before unlock (docs only say CE is unavailable; `SQLITE_CANTOPEN` reports are empirical).
5. A documentation sentence stating explicitly that `setAlarmClock` fires while in Direct Boot (inferred from architecture + Doze exemption + the Direct Boot guide's alarm-clock example).
6. Attribution of FGS starts made from a `LOCKED_BOOT_COMPLETED` receiver on Android 15+ (the `BOOT_COMPLETED` mediaPlayback restriction is documented; whether `LOCKED_BOOT_COMPLETED` carries the same restriction is not explicitly documented — device-test it).
7. Any Android 16 `mediaPlayback`-specific restriction (official pages list none; blog claims unverified).
8. Any documented connection between Android 16 Live Updates and alarm APIs.
9. A separate Play policy page restricting `SCHEDULE_EXACT_ALARM` beyond the general restricted-permissions language.
