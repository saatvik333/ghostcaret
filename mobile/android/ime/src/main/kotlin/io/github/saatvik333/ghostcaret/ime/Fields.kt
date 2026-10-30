package io.github.saatvik333.ghostcaret.ime

import android.text.InputType
import android.view.inputmethod.EditorInfo

/** The engine's field kind for an editor, from its input type. */
internal fun fieldKind(info: EditorInfo): Int {
    val type = info.inputType
    val variation = type and InputType.TYPE_MASK_VARIATION
    return when (type and InputType.TYPE_MASK_CLASS) {
        InputType.TYPE_CLASS_TEXT -> when (variation) {
            InputType.TYPE_TEXT_VARIATION_PASSWORD,
            InputType.TYPE_TEXT_VARIATION_VISIBLE_PASSWORD,
            InputType.TYPE_TEXT_VARIATION_WEB_PASSWORD -> Codes.Kind.PASSWORD
            InputType.TYPE_TEXT_VARIATION_URI -> Codes.Kind.URL
            InputType.TYPE_TEXT_VARIATION_EMAIL_ADDRESS,
            InputType.TYPE_TEXT_VARIATION_WEB_EMAIL_ADDRESS -> Codes.Kind.EMAIL
            InputType.TYPE_TEXT_VARIATION_FILTER -> Codes.Kind.SEARCH
            else -> if (info.imeOptions and EditorInfo.IME_MASK_ACTION == EditorInfo.IME_ACTION_SEARCH) {
                Codes.Kind.SEARCH
            } else {
                Codes.Kind.TEXT
            }
        }
        InputType.TYPE_CLASS_NUMBER ->
            if (variation == InputType.TYPE_NUMBER_VARIATION_PASSWORD) Codes.Kind.PIN else Codes.Kind.NUMBER
        InputType.TYPE_CLASS_PHONE -> Codes.Kind.PHONE
        InputType.TYPE_CLASS_DATETIME -> Codes.Kind.NUMBER
        else -> Codes.Kind.UNKNOWN
    }
}

/**
 * Fields where Ghostcaret never reads, predicts or shows anything. Mirrors
 * `FieldKind::is_secure_or_structured`; the keyboard checks it itself so
 * their text never even leaves the editor.
 */
internal fun isQuiet(kind: Int): Boolean = when (kind) {
    Codes.Kind.PASSWORD, Codes.Kind.PIN, Codes.Kind.URL, Codes.Kind.NUMBER, Codes.Kind.PHONE -> true
    else -> false
}

internal fun isMultiline(info: EditorInfo): Boolean =
    info.inputType and InputType.TYPE_MASK_CLASS == InputType.TYPE_CLASS_TEXT &&
        info.inputType and InputType.TYPE_TEXT_FLAG_MULTI_LINE != 0

private fun utf8Length(codePoint: Int): Int = when {
    codePoint < 0x80 -> 1
    codePoint < 0x800 -> 2
    codePoint < 0x10000 -> 3
    else -> 4
}

/**
 * UTF-16 units at the end of [text] that hold [bytes] UTF-8 bytes. Stops at a
 * code point boundary and never past the start.
 */
internal fun utf16Before(text: CharSequence, bytes: Int): Int {
    var units = 0
    var left = bytes
    var i = text.length
    while (left > 0 && i > 0) {
        val cp = Character.codePointBefore(text, i)
        val n = Character.charCount(cp)
        left -= utf8Length(cp)
        if (left < 0) break
        i -= n
        units += n
    }
    return units
}

/** UTF-16 units at the start of [text] that hold [bytes] UTF-8 bytes. */
internal fun utf16After(text: CharSequence, bytes: Int): Int {
    var left = bytes
    var i = 0
    while (left > 0 && i < text.length) {
        val cp = Character.codePointAt(text, i)
        left -= utf8Length(cp)
        if (left < 0) break
        i += Character.charCount(cp)
    }
    return i
}

/**
 * Drops a surrogate half that a length cap split off at either end, so the
 * text converts to UTF-8 exactly.
 */
internal fun wholeCodePoints(text: CharSequence): String {
    var from = 0
    var to = text.length
    if (to > 0 && Character.isLowSurrogate(text[0])) from = 1
    if (to > from && Character.isHighSurrogate(text[to - 1])) to -= 1
    return text.subSequence(from, to).toString()
}
