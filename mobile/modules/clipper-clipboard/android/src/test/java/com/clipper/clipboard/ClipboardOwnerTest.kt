package com.clipper.clipboard

import android.content.ClipData
import android.content.ClipboardManager
import android.content.Context
import org.junit.Assert.*
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.RobolectricTestRunner
import org.robolectric.RuntimeEnvironment
import org.robolectric.annotation.Config

@RunWith(RobolectricTestRunner::class)
@Config(sdk = [35])
class ClipboardOwnerTest {
    private lateinit var context: Context
    private lateinit var clipboard: ClipboardManager
    private lateinit var owner: ClipboardOwner

    @Before
    fun setup() {
        context = RuntimeEnvironment.getApplication()
        clipboard = context.getSystemService(Context.CLIPBOARD_SERVICE) as ClipboardManager
        context.getSharedPreferences("clipper-clipboard", Context.MODE_PRIVATE).edit().clear().commit()
        owner = ClipboardOwner(context)
    }

    private fun copy(text: String, timestamp: Long) {
        val clip = ClipData.newPlainText("Other app", text)
        clipboard.setPrimaryClip(clip)
        val description = clipboard.primaryClipDescription!!
        description.javaClass.getDeclaredMethod("setTimestamp", Long::class.javaPrimitiveType)
            .invoke(description, timestamp)
    }

    @Test
    fun installedContentClearsAfterPurgeAndRestart() {
        owner.install("item", "secret", "device")
        ClipboardOwner(context).clearMissing(emptyList(), "device")
        assertFalse(clipboard.hasPrimaryClip())
    }

    @Test
    fun capturedContentClearsOnlyWhenItsItemDisappears() {
        copy("secret", 100)
        owner.claim("item", 100.0, "device")
        owner.clearMissing(listOf("item"), "device")
        assertTrue(clipboard.hasPrimaryClip())
        owner.clearMissing(emptyList(), "device")
        assertFalse(clipboard.hasPrimaryClip())
    }

    @Test
    fun laterCopiesAreKeptEvenWithTheSameText() {
        for (text in listOf("later", "secret")) {
            copy("secret", 100)
            owner.claim("item", 100.0, "device")
            copy(text, 200)
            owner.clearMissing(emptyList(), "device")
            assertEquals(text, clipboard.primaryClip!!.getItemAt(0).text.toString())
        }
    }

    @Test
    fun anotherAccountDoesNotClearTheClipboard() {
        owner.install("item", "secret", "device")
        owner.clearMissing(emptyList(), "another-device")
        assertTrue(clipboard.hasPrimaryClip())
    }

    @Test
    fun aCaptureThatChangedDuringUploadCannotClaimTheLaterCopy() {
        copy("secret", 100)
        copy("later", 200)
        owner.claim("item", 100.0, "device")
        owner.clearMissing(emptyList(), "device")
        assertEquals("later", clipboard.primaryClip!!.getItemAt(0).text.toString())
    }
}
