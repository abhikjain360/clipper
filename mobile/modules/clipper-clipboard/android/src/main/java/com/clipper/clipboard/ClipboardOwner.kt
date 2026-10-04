package com.clipper.clipboard

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import android.os.Build
import android.os.PersistableBundle
import java.util.UUID

class ClipboardOwner(context: Context) {
    private val clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
    private val preferences = context.getSharedPreferences("clipper-clipboard", Context.MODE_PRIVATE)

    fun read(): Map<String, Any> {
        val clip = clipboard.primaryClip
        return mapOf(
            "text" to (clip?.getItemAt(0)?.text?.toString() ?: ""),
            "timestamp" to (clip?.description?.timestamp ?: -1L).toDouble(),
        )
    }

    fun claim(id: String, timestamp: Double, scope: String) {
        if (clipboard.primaryClipDescription?.timestamp == timestamp.toLong()) {
            save(id, timestamp.toLong(), scope, null)
        }
    }

    fun install(id: String, text: String, scope: String) {
        val token = UUID.randomUUID().toString()
        val clip = ClipData.newPlainText("Clipper", text)
        clip.description.extras = PersistableBundle().apply { putString("clipper_copy", token) }
        clipboard.setPrimaryClip(clip)
        val current = clipboard.primaryClipDescription
        if (current?.extras?.getString("clipper_copy") == token) {
            save(id, current.timestamp, scope, token)
        }
    }

    fun clearMissing(ids: List<String>, scope: String) {
        if (preferences.getString("scope", null) != scope) return
        val id = preferences.getString("id", null) ?: return
        if (id in ids) return
        val timestamp = preferences.getLong("timestamp", -1L)
        val current = clipboard.primaryClipDescription ?: return
        val token = preferences.getString("token", null)
        if (current.timestamp == timestamp && (token == null || current.extras?.getString("clipper_copy") == token)) {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                clipboard.clearPrimaryClip()
            } else {
                clipboard.setPrimaryClip(ClipData.newPlainText("", ""))
            }
        }
        preferences.edit().clear().apply()
    }

    private fun save(id: String, timestamp: Long, scope: String, token: String?) {
        preferences.edit().putString("id", id).putLong("timestamp", timestamp)
            .putString("scope", scope).putString("token", token).apply()
    }
}
