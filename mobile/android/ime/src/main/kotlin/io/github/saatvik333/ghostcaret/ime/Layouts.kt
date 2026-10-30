package io.github.saatvik333.ghostcaret.ime

internal enum class KeyType { CHAR, SHIFT, BACKSPACE, ENTER, SPACE, LETTERS, SYMBOLS, MORE_SYMBOLS }

/**
 * One key. [text] is what a character key types, in lower case on the
 * letters page; [width] is relative, ten units per row.
 */
internal class KeyDef(
    val type: KeyType,
    val text: String = "",
    val width: Float = 1f,
    val alternates: List<String> = emptyList(),
)

internal enum class Page { LETTERS, SYMBOLS, MORE_SYMBOLS }

/** Long-press alternates, first entry shown as the key's hint. */
private val ALTERNATES: Map<String, String> = mapOf(
    "q" to "1", "w" to "2", "e" to "3èéêëēęė", "r" to "4", "t" to "5", "y" to "6ýÿ",
    "u" to "7ùúûüū", "i" to "8ìíîïīį", "o" to "9òóôöõœøō", "p" to "0",
    "a" to "àáâäæãåā", "s" to "ßśš", "d" to "ð", "g" to "ğ", "l" to "ł",
    "z" to "žźż", "c" to "çćč", "n" to "ñń",
    "." to ",?!'\":;-…", "1" to "¹½⅓¼⅛", "2" to "²⅔", "3" to "³¾⅜", "4" to "⁴", "5" to "⅝",
    "7" to "⅞", "0" to "ⁿ∅", "$" to "€£¥¢₹₩", "-" to "–—_", "'" to "‘’‚‹›", "\"" to "“”„«»",
    "?" to "¿", "!" to "¡", "*" to "★†‡", "+" to "±", "(" to "<{[", ")" to ">}]", "%" to "‰",
    "=" to "≠≈∞", "/" to "÷", "&" to "§",
)

private fun codePoints(s: String): List<String> =
    s.codePoints().toArray().map { String(Character.toChars(it)) }

private fun char(text: String, width: Float = 1f) =
    KeyDef(KeyType.CHAR, text, width, ALTERNATES[text]?.let(::codePoints).orEmpty())

private fun chars(row: String) = codePoints(row).map { char(it) }

private fun bottomRow(mode: KeyDef, left: String, right: String) = listOf(
    mode,
    char(left),
    KeyDef(KeyType.SPACE, " ", 5f),
    char(right),
    KeyDef(KeyType.ENTER, width = 1.5f),
)

/**
 * The rows of [page]. On the letters page [comma] replaces the comma key:
 * "@" in email fields and "/" in address fields, where those are typed far
 * more often.
 */
internal fun layout(page: Page, comma: String): List<List<KeyDef>> = when (page) {
    Page.LETTERS -> listOf(
        chars("qwertyuiop"),
        chars("asdfghjkl"),
        listOf(KeyDef(KeyType.SHIFT, width = 1.5f)) + chars("zxcvbnm") + KeyDef(KeyType.BACKSPACE, width = 1.5f),
        bottomRow(KeyDef(KeyType.SYMBOLS, width = 1.5f), comma, "."),
    )
    Page.SYMBOLS -> listOf(
        chars("1234567890"),
        chars("@#\$_&-+()/"),
        listOf(KeyDef(KeyType.MORE_SYMBOLS, width = 1.5f)) + chars("*\"':;!?") + KeyDef(KeyType.BACKSPACE, width = 1.5f),
        bottomRow(KeyDef(KeyType.LETTERS, width = 1.5f), ",", "."),
    )
    Page.MORE_SYMBOLS -> listOf(
        chars("~`|•√π÷×¶∆"),
        chars("£¢€¥^°={}\\"),
        listOf(KeyDef(KeyType.SYMBOLS, width = 1.5f)) + chars("%©®™✓[]") + KeyDef(KeyType.BACKSPACE, width = 1.5f),
        bottomRow(KeyDef(KeyType.LETTERS, width = 1.5f), "<", ">"),
    )
}
