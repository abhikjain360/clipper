package com.clipper.clipboard

import expo.modules.kotlin.modules.Module
import expo.modules.kotlin.modules.ModuleDefinition

class ClipperClipboardModule : Module() {
    private val owner by lazy { ClipboardOwner(requireNotNull(appContext.reactContext)) }

    override fun definition() = ModuleDefinition {
        Name("ClipperClipboard")

        Function("read") {
            owner.read()
        }

        Function("claim") { id: String, timestamp: Double, scope: String, token: String ->
            owner.claim(id, timestamp, scope, token)
        }

        Function("install") { id: String, text: String, scope: String ->
            owner.install(id, text, scope)
        }

        Function("clearDeleted") { ids: List<String>, scope: String ->
            owner.clearDeleted(ids, scope)
        }

        Function("reset") {
            owner.reset()
        }
    }
}
