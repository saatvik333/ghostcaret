package io.github.saatvik333.ghostcaret.ime

/**
 * Integer codes shared with the engine. They mirror
 * `crates/platform/src/android/text.rs` and `bridge.rs`; change both together.
 */
internal object Codes {
    object Kind {
        const val TEXT = 0
        const val PASSWORD = 1
        const val PIN = 2
        const val SEARCH = 3
        const val URL = 4
        const val EMAIL = 5
        const val NUMBER = 6
        const val PHONE = 7
        const val TERMINAL = 8
        const val CODE = 9
        const val UNKNOWN = 10
    }

    object Dismiss {
        const val ESCAPE = 0
        const val MISMATCH = 1
        const val CARET_MOVED = 2
        const val FOCUS_LOST = 3
        const val CLICKED = 4
    }

    /** Also the order of the binding slots the engine sends. */
    object Action {
        const val TAKE_WORD = 0
        const val TAKE_ALL = 1
        const val SHOW_MORE = 2
        const val NEXT_ALTERNATIVE = 3
        const val PREV_ALTERNATIVE = 4
        const val DISMISS = 5
        const val FLOW_DRAFT = 6
        const val COUNT = 7
    }

    object CardKey {
        const val UP = 0
        const val DOWN = 1
        const val PICK = 2
        const val ENTER = 3
        const val ESCAPE = 4
        const val TAB = 5
        const val BACKSPACE = 6
        const val CHAR = 7
    }

    object Replace {
        const val NOTHING = 0
        const val SELECTION = 1
        const val AROUND = 2
    }

    object Insert {
        const val INSERTED = 0
        const val FAILED = 1
        const val UNKNOWN = 2
    }

    object Status {
        const val CLEAR = 0
        const val PAUSED = 1
        const val OFF_IN_APP = 2
        const val NO_MODEL = 3
        const val CLOUD_UNAVAILABLE = 4
        const val LOADING = 5
        const val LOAD_FAILED = 6
        const val SETTINGS_INVALID = 7
    }
}
