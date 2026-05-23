package com.clipper.clipper_app

import android.content.ClipData
import android.content.ClipDescription
import android.content.ClipboardManager
import android.content.Context
import android.os.Build
import android.os.PersistableBundle
import io.flutter.embedding.android.FlutterActivity
import io.flutter.embedding.engine.FlutterEngine
import io.flutter.plugin.common.MethodChannel

class MainActivity : FlutterActivity() {
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
}
