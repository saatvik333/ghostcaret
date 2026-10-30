package io.github.saatvik333.ghostcaret.ime

import android.util.Log

/**
 * The engine's JNI entry points (`crates/platform/src/android/exports.rs`).
 * Call them on the UI thread only; none of them blocks. A handle from
 * [start] stays valid until it is passed to [stop], and must not be used
 * afterwards.
 */
internal object Native {
    /**
     * The library the engine is linked into: the Tauri app's library, which
     * the standalone harness builds under the same name.
     */
    private const val LIBRARY = "open_flowtype_lib"

    /** False when the library is missing; the keyboard then types without suggestions. */
    val available: Boolean = try {
        System.loadLibrary(LIBRARY)
        true
    } catch (e: UnsatisfiedLinkError) {
        Log.e(TAG, "engine library $LIBRARY unavailable", e)
        false
    }

    /** Returns 0 when the engine cannot start. */
    @JvmStatic external fun start(bridge: Bridge, configDir: String, dataDir: String, trace: Boolean): Long
    @JvmStatic external fun stop(handle: Long)

    @JvmStatic external fun focus(
        handle: Long, field: Long, appId: String, appName: String, kind: Int, multiline: Boolean,
        before: String, beforeTruncated: Boolean, selection: String, after: String,
    )

    @JvmStatic external fun changed(
        handle: Long, field: Long, revision: Long,
        before: String, beforeTruncated: Boolean, selection: String, after: String,
    )

    @JvmStatic external fun blur(handle: Long)
    @JvmStatic external fun accepted(handle: Long, field: Long, text: String, whole: Boolean)
    @JvmStatic external fun typedThrough(handle: Long, field: Long, text: String)
    @JvmStatic external fun dismissed(handle: Long, field: Long, reason: Int)
    @JvmStatic external fun action(handle: Long, action: Int)
    @JvmStatic external fun cardKey(handle: Long, key: Int, value: Int)
    @JvmStatic external fun insertDone(handle: Long, id: Long, outcome: Int, message: String?)
    @JvmStatic external fun reloadSettings(handle: Long)
    @JvmStatic external fun memoryPressure(handle: Long)

    /** Chooses a local GGUF model in the settings; null on success, else why not. */
    @JvmStatic external fun useLocalModel(configDir: String, model: String): String?

    const val TAG = "oft"
}
