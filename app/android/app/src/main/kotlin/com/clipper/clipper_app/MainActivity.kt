package com.clipper.clipper_app

import android.Manifest
import android.content.ClipData
import android.content.ClipDescription
import android.content.ClipboardManager
import android.content.Context
import android.content.pm.PackageManager
import android.net.Uri
import android.os.Build
import android.os.PersistableBundle
import androidx.core.content.FileProvider
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel
import java.io.File

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
                "setEntry" -> {
                    val mimeType = call.argument<String>("mimeType")
                    val bytes = call.argument<ByteArray>("bytes")
                    val text = call.argument<String>("text")
                    if (mimeType == null || bytes == null) {
                        result.error("invalid_args", "Missing clipboard payload", null)
                        return@setMethodCallHandler
                    }

                    try {
                        setClipboardEntry(mimeType, bytes, text)
                        result.success(null)
                    } catch (error: RuntimeException) {
                        result.error("clipboard_write_failed", error.message, null)
                    }
                }
                "getEntry" -> {
                    try {
                        result.success(readClipboardEntry())
                    } catch (error: RuntimeException) {
                        result.error("clipboard_read_failed", error.message, null)
                    }
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
        markClipSensitive(clip)
        clipboard.setPrimaryClip(clip)
    }

    private fun setClipboardEntry(mimeType: String, bytes: ByteArray, text: String?) {
        if (mimeType.startsWith("text/")) {
            setSensitiveClipboardText(text ?: bytes.toString(Charsets.UTF_8))
            return
        }

        if (mimeType.startsWith("image/")) {
            val clipboard = getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
            val uri = writeClipboardCacheFile(bytes, extensionForMimeType(mimeType))
            val clip = ClipData.newUri(contentResolver, "Clipper", uri)
            markClipSensitive(clip)
            clipboard.setPrimaryClip(clip)
            return
        }

        throw IllegalArgumentException("Unsupported clipboard MIME type: $mimeType")
    }

    private fun readClipboardEntry(): Map<String, Any?>? {
        val clipboard = getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        val clip = clipboard.primaryClip ?: return null
        if (clip.itemCount == 0) return null

        val description = clip.description
        val item = clip.getItemAt(0)

        val imageUri = item.uri ?: item.intent?.data
        if (imageUri != null && description.hasMimeType("image/*")) {
            val bytes = readUriBytes(imageUri)
            val mimeType = contentResolver.getType(imageUri)
                ?: firstMimeType(description, "image/")
                ?: "image/png"
            return mapOf(
                "mimeType" to normalizeSupportedImageMimeType(mimeType),
                "bytes" to bytes,
                "text" to null
            )
        }

        val text = item.text?.toString()
            ?: item.coerceToText(this)?.toString()
            ?: return null
        if (text.isEmpty()) return null
        return mapOf(
            "mimeType" to "text/plain",
            "bytes" to text.toByteArray(Charsets.UTF_8),
            "text" to text
        )
    }

    private fun markClipSensitive(clip: ClipData) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.N) {
            val extras = PersistableBundle()
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.TIRAMISU) {
                extras.putBoolean(ClipDescription.EXTRA_IS_SENSITIVE, true)
            } else {
                extras.putBoolean("android.content.extra.IS_SENSITIVE", true)
            }
            clip.description.extras = extras
        }
    }

    private fun writeClipboardCacheFile(bytes: ByteArray, extension: String): Uri {
        val dir = File(cacheDir, "clipboard")
        if (!dir.exists()) {
            dir.mkdirs()
        }
        val file = File(dir, "clipper-clipboard.$extension")
        file.writeBytes(bytes)
        return FileProvider.getUriForFile(this, "$packageName.fileprovider", file)
    }

    private fun readUriBytes(uri: Uri): ByteArray {
        return contentResolver.openInputStream(uri)?.use { it.readBytes() }
            ?: throw IllegalArgumentException("Cannot read clipboard URI")
    }

    private fun firstMimeType(description: ClipDescription, prefix: String): String? {
        for (i in 0 until description.mimeTypeCount) {
            val mimeType = description.getMimeType(i)
            if (mimeType.startsWith(prefix)) return mimeType
        }
        return null
    }

    private fun normalizeSupportedImageMimeType(mimeType: String): String {
        return when (mimeType.lowercase()) {
            "image/png" -> "image/png"
            "image/jpeg", "image/jpg" -> "image/jpeg"
            "image/gif" -> "image/gif"
            "image/webp" -> "image/webp"
            else -> throw IllegalArgumentException(
                "Unsupported image clipboard MIME type: $mimeType"
            )
        }
    }

    private fun extensionForMimeType(mimeType: String): String {
        return when (normalizeSupportedImageMimeType(mimeType)) {
            "image/png" -> "png"
            "image/jpeg" -> "jpg"
            "image/gif" -> "gif"
            "image/webp" -> "webp"
            else -> "bin"
        }
    }

    private companion object {
        const val BACKGROUND_SYNC_NOTIFICATION_REQUEST_CODE = 7311
    }
}
