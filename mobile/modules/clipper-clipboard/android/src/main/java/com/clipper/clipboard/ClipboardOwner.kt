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
        val token = if (clip != null && Build.VERSION.SDK_INT < Build.VERSION_CODES.O) mark(clip) else ""
        return mapOf(
            "text" to (clip?.getItemAt(0)?.text?.toString() ?: ""),
            "timestamp" to timestamp().toDouble(),
            "token" to token,
        )
    }

    fun claim(id: String, timestamp: Double, scope: String, token: String) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            if (clipboard.primaryClipDescription?.timestamp == timestamp.toLong()) {
                save(id, timestamp.toLong(), scope, null)
            }
        } else if (token.isNotEmpty() && clipboard.primaryClipDescription?.extras?.getString("clipper_copy") == token) {
            save(id, -1L, scope, token)
        }
    }

    fun install(id: String, text: String, scope: String) {
        val clip = ClipData.newPlainText("Clipper", text)
        val token = mark(clip)
        val current = clipboard.primaryClipDescription
        if (current?.extras?.getString("clipper_copy") == token) {
            save(id, timestamp(), scope, token)
        }
    }

    fun clearDeleted(ids: List<String>, scope: String) {
        if (preferences.getString("scope", null) != scope) {
            reset()
            return
        }
        val id = preferences.getString("id", null) ?: return
        if (id !in ids) return
        val timestamp = preferences.getLong("timestamp", -1L)
        val current = clipboard.primaryClipDescription ?: return
        val token = preferences.getString("token", null)
        val same = if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) {
            current.timestamp == timestamp && (token == null || current.extras?.getString("clipper_copy") == token)
        } else token != null && current.extras?.getString("clipper_copy") == token
        if (same) {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) {
                clipboard.clearPrimaryClip()
            } else {
                clipboard.setPrimaryClip(ClipData.newPlainText("", ""))
            }
        }
        preferences.edit().clear().apply()
    }

    fun reset() {
        preferences.edit().clear().apply()
    }

    private fun timestamp(): Long {
        return if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.O) clipboard.primaryClipDescription?.timestamp ?: -1L else -1L
    }

    private fun mark(clip: ClipData): String {
        val token = UUID.randomUUID().toString()
        val extras = clip.description.extras ?: PersistableBundle()
        extras.putString("clipper_copy", token)
        clip.description.extras = extras
        clipboard.setPrimaryClip(clip)
        return token
    }

    private fun save(id: String, timestamp: Long, scope: String, token: String?) {
        preferences.edit().putString("id", id).putLong("timestamp", timestamp)
            .putString("scope", scope).putString("token", token).apply()
    }
}
