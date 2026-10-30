package io.github.saatvik333.ghostcaret.ime

import android.content.ComponentCallbacks2
import android.content.res.Configuration
import android.inputmethodservice.InputMethodService
import android.os.Build
import android.os.FileObserver
import android.os.Handler
import android.os.Looper
import android.os.SystemClock
import android.provider.Settings
import android.text.InputType
import android.text.TextUtils
import android.util.Log
import android.view.KeyCharacterMap
import android.view.KeyEvent
import android.view.View
import android.view.WindowInsets
import android.view.inputmethod.EditorInfo
import android.view.inputmethod.InputMethodManager
import android.widget.LinearLayout
import java.io.File

/**
 * Ghostcaret's keyboard. It owns the key path: suggestions are applied to
 * the keyboard's own copy on the UI thread, committed, and only then reported
 * to the engine, so text can never be reordered with the keys typed next.
 *
 * The keyboard keeps a copy of the text around the caret. Its own edits are
 * applied to the copy directly; only changes it did not make are read back
 * from the editor. Password, PIN, number, phone and address fields are never
 * read at all.
 */
class KeyboardService : InputMethodService(), KeyboardView.Listener, StripView.Listener, DraftCardView.Listener {
    private val ui = Handler(Looper.getMainLooper())
    private var engine = 0L
    private var trace = false
    private var watcher: FileObserver? = null

    private var keyboard: KeyboardView? = null
    private var strip: StripView? = null
    private var cardView: DraftCardView? = null

    /** True while the engine follows the focused field. */
    private var engaged = false
    private var field = 0L
    private var fields = 0L
    private var revision = 0L
    private var kind = Codes.Kind.UNKNOWN
    private var quiet = true
    private var rawKeys = false
    private var multiline = false
    private var before = ""
    private var beforeTruncated = false
    private var selection = ""
    private var after = ""
    private var selStart = -1
    private var selEnd = -1

    /** Carets the editor will report for edits the copy already has. */
    private val expectedCarets = ArrayDeque<Int>()

    private var suggestion: Suggestion? = null
    private var card: Card? = null
    private var active = true
    private var status: String? = null
    private var highContrast = false
    private var announce = false
    private var bindings = IntArray(Codes.Action.COUNT * 3) { if (it % 3 == 0) -1 else 0 }
    private var modifierDown = -1
    private var modifierDownAt = 0L
    private var editedAt = 0L
    private val appNames = HashMap<String, String>()

    override fun onCreate() {
        super.onCreate()
        trace = Log.isLoggable(Native.TAG, Log.DEBUG)
        if (Native.available) engine = Native.start(Bridge(this), filesDir.path, filesDir.path, trace)
        if (engine == 0L) status = getString(R.string.oft_status_engine_missing)
        watcher = SettingsWatcher(filesDir) { ui.post { if (engine != 0L) Native.reloadSettings(engine) } }
            .also { it.startWatching() }
    }

    override fun onDestroy() {
        watcher?.stopWatching()
        val handle = engine
        engine = 0L
        engaged = false
        if (handle != 0L) Native.stop(handle)
        super.onDestroy()
    }

    @Suppress("DEPRECATION")
    override fun onTrimMemory(level: Int) {
        super.onTrimMemory(level)
        val pressure = level == ComponentCallbacks2.TRIM_MEMORY_RUNNING_LOW ||
            level == ComponentCallbacks2.TRIM_MEMORY_RUNNING_CRITICAL ||
            level >= ComponentCallbacks2.TRIM_MEMORY_BACKGROUND
        if (pressure && engine != 0L) Native.memoryPressure(engine)
    }

    override fun onEvaluateFullscreenMode() = false

    override fun onCreateInputView(): View {
        val palette = Palette.of(this)
        val keys = KeyboardView(this, this).also { it.palette = palette }
        val strip = StripView(this, this).also {
            it.applyPalette(palette)
            it.highContrast = highContrast
            it.announce = announce
        }
        val card = DraftCardView(this, this).also {
            it.applyPalette(palette)
            it.visibility = View.GONE
        }
        keyboard = keys
        this.strip = strip
        cardView = card
        return LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            setBackgroundColor(palette.background)
            addView(strip, LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT)
            addView(card, LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT)
            addView(keys, LinearLayout.LayoutParams.MATCH_PARENT, LinearLayout.LayoutParams.WRAP_CONTENT)
            setOnApplyWindowInsetsListener { v, insets ->
                v.setPadding(0, 0, 0, navigationBarHeight(insets))
                insets
            }
        }
    }

    private fun navigationBarHeight(insets: WindowInsets): Int =
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            insets.getInsets(WindowInsets.Type.navigationBars()).bottom
        } else {
            @Suppress("DEPRECATION")
            insets.systemWindowInsetBottom
        }

    // Field lifecycle

    override fun onStartInput(info: EditorInfo, restarting: Boolean) {
        super.onStartInput(info, restarting)
        suggestion = null
        card = null
        field = ++fields
        revision = 0
        kind = fieldKind(info)
        quiet = isQuiet(kind)
        rawKeys = info.inputType == InputType.TYPE_NULL
        multiline = isMultiline(info)
        selStart = info.initialSelStart
        selEnd = info.initialSelEnd
        expectedCarets.clear()
        before = ""
        beforeTruncated = false
        selection = ""
        after = ""
        // Editors without an input type have no text to read or predict.
        engaged = engine != 0L && !rawKeys
        refreshStrip()
        if (!engaged) return
        if (!quiet && active) readInitial(info)
        val app = info.packageName.orEmpty()
        Native.focus(
            engine, field, app, appName(app), kind, multiline, before, beforeTruncated, selection, after,
        )
    }

    override fun onFinishInput() {
        if (engaged) Native.blur(engine)
        engaged = false
        suggestion = null
        card = null
        refreshStrip()
        super.onFinishInput()
    }

    override fun onStartInputView(info: EditorInfo, restarting: Boolean) {
        super.onStartInputView(info, restarting)
        val keys = keyboard ?: return
        keys.locale = resources.configuration.locales[0]
        keys.soundOn = Settings.System.getInt(contentResolver, Settings.System.SOUND_EFFECTS_ENABLED, 0) == 1
        keys.page = when (info.inputType and InputType.TYPE_MASK_CLASS) {
            InputType.TYPE_CLASS_NUMBER, InputType.TYPE_CLASS_PHONE, InputType.TYPE_CLASS_DATETIME -> Page.SYMBOLS
            else -> Page.LETTERS
        }
        keys.comma = when (kind) {
            Codes.Kind.EMAIL -> "@"
            Codes.Kind.URL -> "/"
            else -> ","
        }
        val (label, description) = enterKey(info)
        keys.enterLabel = label
        keys.enterDescription = description
        keys.shift = KeyboardView.Shift.OFF
        updateShift()
        refreshStrip()
    }

    private fun enterKey(info: EditorInfo): Pair<String, String> {
        info.actionLabel?.takeIf { it.isNotEmpty() && info.actionId != 0 }?.let { return it.toString() to it.toString() }
        val res = when (enterAction(info)) {
            EditorInfo.IME_ACTION_GO -> R.string.oft_enter_go
            EditorInfo.IME_ACTION_SEARCH -> R.string.oft_enter_search
            EditorInfo.IME_ACTION_SEND -> R.string.oft_enter_send
            EditorInfo.IME_ACTION_NEXT -> R.string.oft_enter_next
            EditorInfo.IME_ACTION_DONE -> R.string.oft_enter_done
            EditorInfo.IME_ACTION_PREVIOUS -> R.string.oft_enter_previous
            else -> return "↵" to getString(R.string.oft_enter_newline)
        }
        return getString(res) to getString(res)
    }

    /** The editor action Enter performs, or IME_ACTION_NONE for a new line or key event. */
    private fun enterAction(info: EditorInfo): Int {
        if (info.imeOptions and EditorInfo.IME_FLAG_NO_ENTER_ACTION != 0) return EditorInfo.IME_ACTION_NONE
        val action = info.imeOptions and EditorInfo.IME_MASK_ACTION
        return if (action == EditorInfo.IME_ACTION_UNSPECIFIED) EditorInfo.IME_ACTION_NONE else action
    }

    private fun appName(app: String): String = appNames.getOrPut(app) {
        try {
            packageManager.getApplicationLabel(packageManager.getApplicationInfo(app, 0)).toString()
        } catch (_: Exception) {
            app
        }
    }

    // Reading the field

    private fun readInitial(info: EditorInfo) {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            val b = info.getInitialTextBeforeCursor(BEFORE_UNITS, 0)
            val a = info.getInitialTextAfterCursor(AFTER_UNITS, 0)
            if (b != null && a != null) {
                before = wholeCodePoints(b)
                beforeTruncated = b.length >= BEFORE_UNITS
                selection = info.getInitialSelectedText(0)?.toString().orEmpty()
                after = wholeCodePoints(a)
                return
            }
        }
        readField()
    }

    private fun readField() {
        val ic = currentInputConnection ?: return
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
            val around = ic.getSurroundingText(BEFORE_UNITS, AFTER_UNITS, 0)
            if (around != null) {
                val text = around.text
                val start = around.selectionStart.coerceIn(0, text.length)
                val end = around.selectionEnd.coerceIn(start, text.length)
                before = wholeCodePoints(text.subSequence(0, start))
                selection = text.subSequence(start, end).toString()
                after = wholeCodePoints(text.subSequence(end, text.length))
                beforeTruncated = around.offset > 0 || (around.offset < 0 && start >= BEFORE_UNITS)
                return
            }
        }
        val b = ic.getTextBeforeCursor(BEFORE_UNITS, 0) ?: ""
        before = wholeCodePoints(b)
        beforeTruncated = b.length >= BEFORE_UNITS
        selection = ic.getSelectedText(0)?.toString().orEmpty()
        after = wholeCodePoints(ic.getTextAfterCursor(AFTER_UNITS, 0) ?: "")
    }

    private fun sendChanged() {
        updateShift()
        if (!engaged || quiet || !active) return
        revision++
        editedAt = SystemClock.elapsedRealtimeNanos()
        Native.changed(engine, field, revision, before, beforeTruncated, selection, after)
    }

    override fun onUpdateSelection(
        oldSelStart: Int, oldSelEnd: Int, newSelStart: Int, newSelEnd: Int, candidatesStart: Int, candidatesEnd: Int,
    ) {
        super.onUpdateSelection(oldSelStart, oldSelEnd, newSelStart, newSelEnd, candidatesStart, candidatesEnd)
        if (newSelStart == newSelEnd && ownEdit(newSelStart)) {
            if (expectedCarets.isEmpty()) {
                selStart = newSelStart
                selEnd = newSelEnd
            }
            return
        }
        // Something other than the keyboard moved the caret or changed the text.
        expectedCarets.clear()
        selStart = newSelStart
        selEnd = newSelEnd
        dismiss(Codes.Dismiss.CARET_MOVED)
        if (engaged && !quiet && active) readField()
        refreshStrip()
        sendChanged()
    }

    private fun ownEdit(caret: Int): Boolean {
        val i = expectedCarets.indexOf(caret)
        if (i < 0) return false
        repeat(i + 1) { expectedCarets.removeFirst() }
        return true
    }

    // Editing

    /** Commits [text] and applies it to the copy. */
    private fun commit(text: String) {
        val ic = currentInputConnection ?: return
        if (rawKeys) {
            for (c in text) sendKeyChar(c)
            return
        }
        ic.commitText(text, 1)
        if (selStart < 0) return
        val caret = minOf(selStart, selEnd) + text.length
        if (expectedCarets.size == MAX_EXPECTED) expectedCarets.removeFirst()
        expectedCarets.addLast(caret)
        selStart = caret
        selEnd = caret
        if (quiet) return
        selection = ""
        before += text
        if (before.length > BEFORE_UNITS) {
            before = wholeCodePoints(before.substring(before.length - BEFORE_UNITS))
            beforeTruncated = true
        }
    }

    private fun dismiss(reason: Int) {
        val s = suggestion ?: return
        suggestion = null
        refreshStrip()
        if (engaged) Native.dismissed(engine, s.field, reason)
    }

    override fun onText(text: String) {
        if (card != null) {
            cardText(text)
            return
        }
        val s = suggestion
        if (s != null && s.typeThrough(text)) {
            commit(text)
            if (s.isEmpty) suggestion = null
            refreshStrip()
            Native.typedThrough(engine, s.field, text)
        } else {
            if (s != null) {
                suggestion = null
                refreshStrip()
            }
            commit(text)
            if (s != null) Native.dismissed(engine, s.field, Codes.Dismiss.MISMATCH)
        }
        sendChanged()
    }

    override fun onBackspace() {
        if (card != null) {
            if (engaged) Native.cardKey(engine, Codes.CardKey.BACKSPACE, 0)
            return
        }
        // The editor deletes the selection or the last character cluster
        // itself; the change comes back through onUpdateSelection.
        val s = suggestion
        suggestion = null
        refreshStrip()
        sendDownUpKeyEvents(KeyEvent.KEYCODE_DEL)
        if (s != null) Native.dismissed(engine, s.field, Codes.Dismiss.MISMATCH)
    }

    override fun onEnter() {
        if (card != null) {
            if (engaged) Native.cardKey(engine, Codes.CardKey.ENTER, 0)
            return
        }
        val info = currentInputEditorInfo ?: return
        val action = enterAction(info)
        when {
            rawKeys -> sendDownUpKeyEvents(KeyEvent.KEYCODE_ENTER)
            info.actionLabel != null && info.actionId != 0 && action != EditorInfo.IME_ACTION_NONE -> {
                dismiss(Codes.Dismiss.MISMATCH)
                currentInputConnection?.performEditorAction(info.actionId)
            }
            action != EditorInfo.IME_ACTION_NONE -> {
                dismiss(Codes.Dismiss.MISMATCH)
                currentInputConnection?.performEditorAction(action)
            }
            multiline -> onText("\n")
            else -> {
                dismiss(Codes.Dismiss.MISMATCH)
                sendDownUpKeyEvents(KeyEvent.KEYCODE_ENTER)
            }
        }
    }

    override fun onShowInputMethodPicker() {
        getSystemService(InputMethodManager::class.java)?.showInputMethodPicker()
    }

    private fun updateShift() {
        val keys = keyboard ?: return
        if (keys.shift == KeyboardView.Shift.LOCKED || keys.page != Page.LETTERS) return
        val caps = (currentInputEditorInfo?.inputType ?: 0) and
            (InputType.TYPE_TEXT_FLAG_CAP_CHARACTERS or InputType.TYPE_TEXT_FLAG_CAP_WORDS or InputType.TYPE_TEXT_FLAG_CAP_SENTENCES)
        val on = caps != 0 && !quiet && TextUtils.getCapsMode(before, before.length, caps) != 0
        keys.shift = if (on) KeyboardView.Shift.ONCE else KeyboardView.Shift.OFF
    }

    // The strip and the card

    override fun onStripAction(action: Int) {
        if (!engaged) return
        val s = suggestion
        when (action) {
            Codes.Action.TAKE_WORD -> if (s != null) s.takeWord()?.let { accept(s, it, whole = false) }
            Codes.Action.TAKE_ALL -> if (s != null) s.takeAll()?.let { accept(s, it, whole = true) }
            Codes.Action.SHOW_MORE -> if (s != null && s.showMore()) refreshStrip() else Native.action(engine, action)
            Codes.Action.DISMISS -> dismiss(Codes.Dismiss.ESCAPE)
            else -> Native.action(engine, action)
        }
    }

    private fun accept(s: Suggestion, text: String, whole: Boolean) {
        commit(text)
        if (s.isEmpty) suggestion = null
        refreshStrip()
        Native.accepted(engine, s.field, text, whole)
        sendChanged()
    }

    private fun cardText(text: String) {
        if (!engaged) return
        val typing = card?.input != null
        var i = 0
        while (i < text.length) {
            val cp = text.codePointAt(i)
            i += Character.charCount(cp)
            if (!typing && cp in '1'.code..'9'.code) {
                Native.cardKey(engine, Codes.CardKey.PICK, cp - '0'.code)
            } else {
                Native.cardKey(engine, Codes.CardKey.CHAR, cp)
            }
        }
    }

    override fun onCardKey(key: Int, value: Int) {
        if (engaged) Native.cardKey(engine, key, value)
    }

    private fun refreshStrip() {
        val strip = strip ?: return
        val cardView = cardView ?: return
        val c = card
        if (c != null) {
            strip.visibility = View.GONE
            cardView.visibility = View.VISIBLE
            cardView.show(c, hardwareKeyboard())
            return
        }
        cardView.visibility = View.GONE
        strip.visibility = View.VISIBLE
        strip.rewrite = selection.isNotEmpty()
        strip.show(suggestion, if (quiet && engaged) null else status)
    }

    private fun hardwareKeyboard(): Boolean {
        val config = resources.configuration
        return config.keyboard != Configuration.KEYBOARD_NOKEYS &&
            config.hardKeyboardHidden == Configuration.HARDKEYBOARDHIDDEN_NO
    }

    // Calls from the engine, posted by Bridge

    internal fun showSuggestion(s: Suggestion) {
        if (!engaged || !active || card != null || s.field != field || s.revision != revision) return
        suggestion = s
        refreshStrip()
        if (trace) {
            val ms = (SystemClock.elapsedRealtimeNanos() - editedAt) / 1e6
            Log.d(Native.TAG, "suggestion shown %.1f ms after the last edit".format(ms))
        }
    }

    internal fun showCard(c: Card) {
        if (!engaged || c.field != field) return
        suggestion = null
        card = c
        refreshStrip()
    }

    internal fun hideOverlay() {
        suggestion = null
        card = null
        refreshStrip()
    }

    internal fun insert(id: Long, target: Long, text: String, replace: Int, beforeBytes: Int, afterBytes: Int) {
        if (!engaged) return
        val ic = currentInputConnection
        if (target != field || ic == null) {
            Native.insertDone(engine, id, Codes.Insert.FAILED, getString(R.string.oft_insert_field_changed))
            return
        }
        ic.beginBatchEdit()
        when (replace) {
            Codes.Replace.AROUND -> ic.deleteSurroundingText(utf16Before(before, beforeBytes), utf16After(after, afterBytes))
            Codes.Replace.NOTHING -> if (selStart != selEnd && selEnd >= 0) ic.setSelection(selEnd, selEnd)
        }
        val committed = ic.commitText(text, 1)
        ic.endBatchEdit()
        if (committed) {
            Native.insertDone(engine, id, Codes.Insert.INSERTED, null)
        } else {
            Native.insertDone(engine, id, Codes.Insert.FAILED, getString(R.string.oft_insert_rejected))
        }
    }

    internal fun setActive(on: Boolean) {
        if (active == on) return
        active = on
        if (!on) {
            hideOverlay()
        } else if (engaged && !quiet) {
            readField()
            sendChanged()
        }
    }

    internal fun setBindings(slots: IntArray) {
        if (slots.size == Codes.Action.COUNT * 3) bindings = slots
    }

    internal fun setAppearance(highContrast: Boolean, announce: Boolean) {
        this.highContrast = highContrast
        this.announce = announce
        strip?.let {
            it.highContrast = highContrast
            it.announce = announce
        }
        refreshStrip()
    }

    internal fun showStatus(code: Int, detail: String?) {
        status = when (code) {
            Codes.Status.PAUSED -> getString(R.string.oft_status_paused)
            Codes.Status.OFF_IN_APP -> getString(R.string.oft_status_off_in_app)
            Codes.Status.NO_MODEL -> getString(R.string.oft_status_no_model)
            Codes.Status.CLOUD_UNAVAILABLE -> getString(R.string.oft_status_cloud_unavailable, detail.orEmpty())
            Codes.Status.LOADING -> getString(R.string.oft_status_loading)
            Codes.Status.LOAD_FAILED -> getString(R.string.oft_status_load_failed, detail.orEmpty())
            Codes.Status.SETTINGS_INVALID -> getString(R.string.oft_status_settings_invalid, detail.orEmpty())
            else -> null
        }
        refreshStrip()
    }

    // Hardware keyboards

    override fun onKeyDown(keyCode: Int, event: KeyEvent): Boolean {
        if (!engaged) return super.onKeyDown(keyCode, event)
        if (MODIFIERS.indexOf(keyCode) >= 0) {
            if (event.repeatCount == 0) {
                modifierDown = keyCode
                modifierDownAt = event.eventTime
            }
            return super.onKeyDown(keyCode, event)
        }
        modifierDown = -1
        if (card != null) {
            cardKeyOf(event)?.let { (key, value) -> Native.cardKey(engine, key, value) }
            return true
        }
        val bound = boundAction(event)
        if (bound == Codes.Action.FLOW_DRAFT || (bound >= 0 && suggestion != null)) {
            onStripAction(bound)
            return true
        }
        val typed = printable(event)
        if (typed != null && !quiet) {
            onText(typed)
            return true
        }
        return super.onKeyDown(keyCode, event)
    }

    override fun onKeyUp(keyCode: Int, event: KeyEvent): Boolean {
        if (engaged && keyCode == modifierDown) {
            modifierDown = -1
            val slot = Codes.Action.FLOW_DRAFT * 3
            val tap = event.eventTime - modifierDownAt <= TAP_MAX_MS
            if (tap && bindings[slot] == 2 && MODIFIERS.getOrNull(bindings[slot + 1]) == keyCode) {
                onStripAction(Codes.Action.FLOW_DRAFT)
                return true
            }
        }
        return super.onKeyUp(keyCode, event)
    }

    /** The bound action for a key press, or -1. */
    private fun boundAction(event: KeyEvent): Int {
        val mods = modsOf(event)
        val named = NAMED_KEYS[event.keyCode] ?: (if (event.keyCode in F_KEYS) 100 + event.keyCode - F_KEYS.first + 1 else -1)
        val char = event.getUnicodeChar(event.metaState and SHORTCUT_META.inv())
        for (action in 0 until Codes.Action.COUNT) {
            val kind = bindings[action * 3]
            val value = bindings[action * 3 + 1]
            val mask = bindings[action * 3 + 2]
            val hit = when (kind) {
                0 -> named >= 0 && value == named && mask == mods
                1 -> char != 0 && value == char && mask and SHIFT_BIT.inv() == mods and SHIFT_BIT.inv()
                else -> false
            }
            if (hit) return action
        }
        return -1
    }

    private fun cardKeyOf(event: KeyEvent): Pair<Int, Int>? = when (event.keyCode) {
        KeyEvent.KEYCODE_DPAD_UP -> Codes.CardKey.UP to 0
        KeyEvent.KEYCODE_DPAD_DOWN -> Codes.CardKey.DOWN to 0
        KeyEvent.KEYCODE_ENTER, KeyEvent.KEYCODE_NUMPAD_ENTER -> Codes.CardKey.ENTER to 0
        KeyEvent.KEYCODE_ESCAPE, KeyEvent.KEYCODE_BACK -> Codes.CardKey.ESCAPE to 0
        KeyEvent.KEYCODE_TAB -> Codes.CardKey.TAB to 0
        KeyEvent.KEYCODE_DEL -> Codes.CardKey.BACKSPACE to 0
        else -> printable(event)?.let { text ->
            val cp = text.codePointAt(0)
            if (card?.input == null && cp in '1'.code..'9'.code) Codes.CardKey.PICK to cp - '0'.code else Codes.CardKey.CHAR to cp
        }
    }

    /** What a key types, unless it is a shortcut or a dead key. */
    private fun printable(event: KeyEvent): String? {
        if (event.metaState and SHORTCUT_META != 0) return null
        val c = event.getUnicodeChar(event.metaState)
        if (c == 0 || c and KeyCharacterMap.COMBINING_ACCENT != 0) return null
        return String(Character.toChars(c))
    }

    private fun modsOf(event: KeyEvent): Int =
        (if (event.isCtrlPressed) 1 else 0) or (if (event.isAltPressed) 2 else 0) or
            (if (event.isShiftPressed) SHIFT_BIT else 0) or (if (event.isMetaPressed) 8 else 0)

    @Suppress("DEPRECATION")
    private class SettingsWatcher(dir: File, private val onChange: () -> Unit) :
        FileObserver(dir.path, CLOSE_WRITE or MOVED_TO) {
        override fun onEvent(event: Int, path: String?) {
            if (path == "settings.toml") onChange()
        }
    }

    private companion object {
        /** UTF-16 units read before the caret: at least the engine's 8 KiB of UTF-8. */
        const val BEFORE_UNITS = 8 * 1024
        const val AFTER_UNITS = 2 * 1024
        const val MAX_EXPECTED = 32
        const val TAP_MAX_MS = 400
        const val SHIFT_BIT = 4
        val SHORTCUT_META = KeyEvent.META_CTRL_MASK or KeyEvent.META_ALT_MASK or KeyEvent.META_META_MASK

        /** Modifier key codes, in the engine's modifier order. */
        val MODIFIERS = intArrayOf(
            KeyEvent.KEYCODE_ALT_LEFT, KeyEvent.KEYCODE_ALT_RIGHT,
            KeyEvent.KEYCODE_CTRL_LEFT, KeyEvent.KEYCODE_CTRL_RIGHT,
            KeyEvent.KEYCODE_SHIFT_LEFT, KeyEvent.KEYCODE_SHIFT_RIGHT,
            KeyEvent.KEYCODE_META_LEFT, KeyEvent.KEYCODE_META_RIGHT,
        )

        /** Key codes of the engine's named keys. */
        val NAMED_KEYS = mapOf(
            KeyEvent.KEYCODE_TAB to 0, KeyEvent.KEYCODE_ESCAPE to 1, KeyEvent.KEYCODE_ENTER to 2,
            KeyEvent.KEYCODE_DEL to 3, KeyEvent.KEYCODE_SPACE to 4, KeyEvent.KEYCODE_DPAD_LEFT to 5,
            KeyEvent.KEYCODE_DPAD_RIGHT to 6, KeyEvent.KEYCODE_DPAD_UP to 7, KeyEvent.KEYCODE_DPAD_DOWN to 8,
            KeyEvent.KEYCODE_MOVE_HOME to 9, KeyEvent.KEYCODE_MOVE_END to 10, KeyEvent.KEYCODE_PAGE_UP to 11,
            KeyEvent.KEYCODE_PAGE_DOWN to 12,
        )
        val F_KEYS = KeyEvent.KEYCODE_F1..KeyEvent.KEYCODE_F12
    }
}
