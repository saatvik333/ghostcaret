package io.github.saatvik333.ghostcaret.ime

import android.os.Handler
import android.os.Looper

/**
 * What the engine calls (`crates/platform/src/android/bridge.rs`), on its
 * runtime thread. Each call builds its value off the UI thread and posts it;
 * none waits for the UI. The method names and signatures are looked up by
 * the engine at start.
 */
internal class Bridge(private val service: KeyboardService) {
    private val ui = Handler(Looper.getMainLooper())

    fun showSuggestion(
        field: Long, revision: Long, text: String, shown: Int,
        wordEnds: IntArray, stepEnds: IntArray, alternative: Int, alternatives: Int,
    ) {
        val suggestion = Suggestion(field, revision, text, shown, wordEnds, stepEnds, alternative, alternatives)
        ui.post { service.showSuggestion(suggestion) }
    }

    fun showCard(
        field: Long, title: String, items: Array<String>, selected: Int,
        input: String?, preview: String?, status: String?, hint: String,
    ) {
        val card = Card(field, title, items, selected, input, preview, status, hint)
        ui.post { service.showCard(card) }
    }

    fun hide() {
        ui.post { service.hideOverlay() }
    }

    fun insert(id: Long, field: Long, text: String, replace: Int, before: Int, after: Int) {
        ui.post { service.insert(id, field, text, replace, before, after) }
    }

    fun setActive(active: Boolean) {
        ui.post { service.setActive(active) }
    }

    /** Three integers per action, in [Codes.Action] order: kind, value, modifier mask. */
    fun setBindings(slots: IntArray) {
        ui.post { service.setBindings(slots) }
    }

    fun setAppearance(highContrast: Boolean, announce: Boolean) {
        ui.post { service.setAppearance(highContrast, announce) }
    }

    fun status(code: Int, detail: String?) {
        ui.post { service.showStatus(code, detail) }
    }

    /** Called on the engine's model loader thread; may block briefly. */
    fun apiKey(provider: String): String? = ApiKeys(service).load(provider)
}
