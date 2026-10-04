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
        clipboard.setPrimaryClip(ClipData.newPlainText("Other app", text))
        val description = clipboard.primaryClipDescription!!
        description.javaClass.getDeclaredMethod("setTimestamp", Long::class.javaPrimitiveType)
            .invoke(description, timestamp)
    }

    private fun claim() {
        val captured = owner.read()
        owner.claim("item", captured["timestamp"] as Double, "account", captured["token"] as String)
    }

    @Test
    fun installedContentClearsAfterPurgeAndRestart() {
        owner.install("item", "secret", "account")
        ClipboardOwner(context).clearDeleted(listOf("item"), "account")
        assertFalse(clipboard.hasPrimaryClip())
    }

    @Test
    fun capturedContentClearsOnlyForAnExplicitDelete() {
        copy("secret", 100)
        claim()
        owner.clearDeleted(listOf("other-item"), "account")
        assertTrue(clipboard.hasPrimaryClip())
        owner.clearDeleted(listOf("item"), "account")
        assertFalse(clipboard.hasPrimaryClip())
    }

    @Test
    fun agingOutOfTheNewest100AndAnEmptyStartupListKeepOwnership() {
        owner.install("item", "secret", "account")
        repeat(100) { owner.clearDeleted(emptyList(), "account") }
        ClipboardOwner(context).clearDeleted(emptyList(), "account")
        assertEquals("secret", clipboard.primaryClip!!.getItemAt(0).text.toString())
        owner.clearDeleted(listOf("item"), "account")
        assertFalse(clipboard.hasPrimaryClip())
    }

    @Test
    fun laterCopiesAreKeptEvenWithTheSameText() {
        for (text in listOf("later", "secret")) {
            copy("secret", 100)
            claim()
            copy(text, 200)
            owner.clearDeleted(listOf("item"), "account")
            assertEquals(text, clipboard.primaryClip!!.getItemAt(0).text.toString())
        }
    }

    @Test
    fun accountSwitchAndLogoutDoNotClearTheClipboard() {
        owner.install("item", "secret", "account")
        owner.clearDeleted(listOf("item"), "another-account")
        owner.clearDeleted(listOf("item"), "account")
        assertEquals("secret", clipboard.primaryClip!!.getItemAt(0).text.toString())
        owner.install("item", "secret", "account")
        owner.reset()
        ClipboardOwner(context).clearDeleted(listOf("item"), "account")
        assertEquals("secret", clipboard.primaryClip!!.getItemAt(0).text.toString())
    }

    @Test
    fun aCaptureThatChangedDuringUploadCannotClaimTheLaterCopy() {
        copy("secret", 100)
        val captured = owner.read()
        copy("later", 200)
        owner.claim("item", captured["timestamp"] as Double, "account", captured["token"] as String)
        owner.clearDeleted(listOf("item"), "account")
        assertEquals("later", clipboard.primaryClip!!.getItemAt(0).text.toString())
    }

    @Test
    @Config(sdk = [24, 25])
    fun android7CaptureSurvivesRestartAndKeepsLaterIdenticalCopies() {
        clipboard.setPrimaryClip(ClipData.newPlainText("Other app", "secret"))
        claim()
        ClipboardOwner(context).clearDeleted(listOf("item"), "account")
        assertEquals("", clipboard.primaryClip!!.getItemAt(0).text.toString())
        clipboard.setPrimaryClip(ClipData.newPlainText("Other app", "secret"))
        claim()
        clipboard.setPrimaryClip(ClipData.newPlainText("Other app", "secret"))
        ClipboardOwner(context).clearDeleted(listOf("item"), "account")
        assertEquals("secret", clipboard.primaryClip!!.getItemAt(0).text.toString())
    }

    @Test
    @Config(sdk = [24, 25])
    fun android7InstallAndChangedCaptureKeepCorrectOwnership() {
        owner.install("item", "secret", "account")
        ClipboardOwner(context).clearDeleted(listOf("item"), "account")
        assertEquals("", clipboard.primaryClip!!.getItemAt(0).text.toString())
        clipboard.setPrimaryClip(ClipData.newPlainText("Other app", "secret"))
        val captured = owner.read()
        clipboard.setPrimaryClip(ClipData.newPlainText("Other app", "later"))
        owner.claim("item", captured["timestamp"] as Double, "account", captured["token"] as String)
        owner.clearDeleted(listOf("item"), "account")
        assertEquals("later", clipboard.primaryClip!!.getItemAt(0).text.toString())
    }
}
