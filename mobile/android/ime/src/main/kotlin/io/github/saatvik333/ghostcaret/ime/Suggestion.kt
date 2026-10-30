package io.github.saatvik333.ghostcaret.ime

/**
 * The suggestion as the keyboard shows it and acts on it: the keyboard's copy
 * of the engine's `SuggestionView`, with offsets in UTF-16 units. The engine
 * converts the offsets once when it sends the view. Every operation runs on
 * the UI thread and has the same meaning as its Rust counterpart, so what the
 * keyboard inserts is exactly what the engine expects.
 *
 * Instead of cutting the text on every take, the view keeps a start offset;
 * nothing is copied until a piece is taken.
 */
internal class Suggestion(
    val field: Long,
    val revision: Long,
    private val text: String,
    shown: Int,
    private val wordEnds: IntArray,
    private val stepEnds: IntArray,
    /** 1-based index of this alternative, or 0 when there are none. */
    val alternative: Int,
    val alternatives: Int,
) {
    private var start = 0
    private var end = shown
    private var nextWord = 0
    private var nextStep = 0

    val visible: String get() = text.substring(start, end)

    /** True when nothing is visible: the suggestion is used up. */
    val isEmpty: Boolean get() = start == end

    /** Length of the next Take Word unit within the visible part. */
    val wordLength: Int get() = wordEnd() - start

    private fun wordEnd(): Int {
        while (nextWord < wordEnds.size && wordEnds[nextWord] <= start) nextWord++
        return if (nextWord < wordEnds.size && wordEnds[nextWord] <= end) wordEnds[nextWord] else end
    }

    /** Removes and returns the first word unit within the visible part. */
    fun takeWord(): String? {
        val e = wordEnd()
        if (e == start) return null
        return consume(e)
    }

    /** Removes and returns the whole visible part. */
    fun takeAll(): String? = if (isEmpty) null else consume(end)

    /**
     * Reveals the next step. False when the buffer has no further step; the
     * keyboard then asks the engine for more.
     */
    fun showMore(): Boolean {
        while (nextStep < stepEnds.size && stepEnds[nextStep] <= end) nextStep++
        if (nextStep == stepEnds.size) return false
        end = stepEnds[nextStep++]
        return true
    }

    /**
     * The user typed [typed]. When the visible text starts with it the
     * suggestion advances past it and this returns true; otherwise the
     * suggestion no longer applies.
     */
    fun typeThrough(typed: String): Boolean {
        if (typed.isEmpty() || !text.startsWith(typed, start) || start + typed.length > end) return false
        start += typed.length
        return true
    }

    private fun consume(to: Int): String {
        val taken = text.substring(start, to)
        start = to
        return taken
    }
}
