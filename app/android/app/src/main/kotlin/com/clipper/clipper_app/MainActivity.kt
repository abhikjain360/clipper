package com.clipper.clipper_app

import android.Manifest
import android.content.ClipData
import android.content.ClipDescription
import android.content.ClipboardManager
import android.content.Context
import android.content.pm.PackageManager
import android.os.Build
import android.os.PersistableBundle
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel

class MainActivity : FlutterActivity() {
    private var pendingBackgroundSyncResult: MethodChannel.Result? = null

    override fun configureFlutterEngine(flutterEngine: FlutterEngine) {
        super.configureFlutterEngine(flutterEngine)

        MethodChannel(
            flutterEngine.dartExecutor.binaryMessenger,
            "com.clipper.app/secure_clipboard"
        ).setMethodCallHandler { call, result ->
            when (call.method) {
                "setText" -> {
                    val text = call.argument<String>("text")
                    if (text == null) {
                        result.error("invalid_args", "Missing clipboard text", null)
                        return@setMethodCallHandler
                    }

                    setSensitiveClipboardText(text)
                    result.success(null)
                }
                else -> result.notImplemented()
            }
        }

        MethodChannel(
            flutterEngine.dartExecutor.binaryMessenger,
            "com.clipper.app/background_sync"
        ).setMethodCallHandler { call, result ->
            when (call.method) {
                "start" -> startBackgroundSync(result)
                "stop" -> {
                    SyncForegroundService.stop(this)
                    result.success(null)
                }
                else -> result.notImplemented()
            }
        }
    }

    override fun onRequestPermissionsResult(
        requestCode: Int,
        permissions: Array<out String>,
        grantResults: IntArray
    ) {
        super.onRequestPermissionsResult(requestCode, permissions, grantResults)

        if (requestCode != BACKGROUND_SYNC_NOTIFICATION_REQUEST_CODE) {
            return
        }

        val result = pendingBackgroundSyncResult ?: return
        pendingBackgroundSyncResult = null
        startBackgroundSyncService(result)
    }

    private fun startBackgroundSync(result: MethodChannel.Result) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU &&
            checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) != PackageManager.PERMISSION_GRANTED
        ) {
            if (pendingBackgroundSyncResult != null) {
                result.error(
                    "permission_request_in_progress",
                    "Notification permission request is already in progress",
                    null
                )
                return
            }

            pendingBackgroundSyncResult = result
            requestPermissions(
                arrayOf(Manifest.permission.POST_NOTIFICATIONS),
                BACKGROUND_SYNC_NOTIFICATION_REQUEST_CODE
            )
            return
        }

        startBackgroundSyncService(result)
    }

    private fun startBackgroundSyncService(result: MethodChannel.Result) {
        try {
            SyncForegroundService.start(this)
            result.success(
                Build.VERSION.SDK_INT < Build.VERSION_CODES.TIRAMISU ||
                    checkSelfPermission(Manifest.permission.POST_NOTIFICATIONS) == PackageManager.PERMISSION_GRANTED
            )
        } catch (error: RuntimeException) {
            result.error("background_sync_start_failed", error.message, null)
        }
    }

    private fun setSensitiveClipboardText(text: String) {
        val clipboard = getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        val clip = ClipData.newPlainText("Clipper", text)

        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) {
            val extras = PersistableBundle()
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                extras.putBoolean(ClipDescription.EXTRA_IS_SENSITIVE, true)
            } else {
                extras.putBoolean("android.content.extra.IS_SENSITIVE", true)
            }
            clip.description.extras = extras
        }

        clipboard.setPrimaryClip(clip)
    }

    private companion object {
        const val BACKGROUND_SYNC_NOTIFICATION_REQUEST_CODE = 7311
    }
}
