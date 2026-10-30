package io.github.saatvik333.ghostcaret.ime

import android.content.Context
import java.io.File

/**
 * Model choices for apps that package the keyboard without their own engine
 * settings screen. The keyboard reloads the settings by itself when they change.
 */
object ModelSettings {
    /** Where the keyboard looks for local models: `<files>/models`. */
    fun modelsDir(context: Context): File = File(context.filesDir, "models")

    /**
     * Makes [model], a GGUF file, the local model for autocomplete and for
     * Reply & rewrite. Returns null on success, or the reason it failed.
     */
    fun useLocalModel(context: Context, model: File): String? {
        if (!Native.available) return context.getString(R.string.oft_status_engine_missing)
        return Native.useLocalModel(context.filesDir.path, model.path)
    }
}
