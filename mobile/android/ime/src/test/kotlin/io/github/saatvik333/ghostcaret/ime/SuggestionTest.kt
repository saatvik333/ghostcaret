package io.github.saatvik333.ghostcaret.ime

import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import kotlin.random.Random

class SuggestionTest {
    /**
     * The engine's `SuggestionView` operations, written the way the contract
     * states them (cut the text, shift every offset), to check the keyboard's
     * offset-based copy against.
     */
    private class Reference(var text: String, var shown: Int, wordEnds: IntArray, stepEnds: IntArray) {
        var words = wordEnds.toMutableList()
        var steps = stepEnds.toMutableList()

        fun takeWord(): String? {
            val end = words.firstOrNull { it <= shown } ?: shown
            if (end == 0) return null
            return consume(end)
        }

        fun takeAll(): String? = if (shown == 0) null else consume(shown)

        fun showMore(): Boolean {
            val i = steps.indexOfFirst { it > shown }
            if (i < 0) return false
            shown = steps[i]
            steps = steps.drop(i + 1).toMutableList()
            return true
        }

        fun typeThrough(typed: String): Boolean {
            if (typed.isEmpty() || !text.substring(0, shown).startsWith(typed)) return false
            consume(typed.length)
            return true
        }

        private fun consume(n: Int): String {
            val taken = text.substring(0, n)
            text = text.substring(n)
            shown -= n
            words = words.filter { it > n }.map { it - n }.toMutableList()
            steps = steps.filter { it > n }.map { it - n }.toMutableList()
            return taken
        }
    }

    private val alphabet = listOf("a", "b", " ", ",", "é", "😀", "\n", "中")

    /** A random view whose offsets fall on code point boundaries, as the engine sends them. */
    private fun randomView(rng: Random): Triple<String, Int, Pair<IntArray, IntArray>> {
        val text = buildString { repeat(rng.nextInt(0, 24)) { append(alphabet.random(rng)) } }
        val bounds = (0..text.length).filter { it == text.length || !Character.isLowSurrogate(text[it]) }
        val shown = bounds.random(rng)
        val words = bounds.filter { it > 0 && rng.nextInt(3) == 0 }.toIntArray()
        val steps = bounds.filter { it > shown && rng.nextInt(3) == 0 }.toIntArray()
        return Triple(text, shown, words to steps)
    }

    @Test
    fun matchesTheContractOnRandomOperations() {
        val rng = Random(7)
        repeat(5000) {
            val (text, shown, ends) = randomView(rng)
            val view = Suggestion(1, 1, text, shown, ends.first, ends.second, 0, 0)
            val reference = Reference(text, shown, ends.first, ends.second)
            repeat(12) {
                when (rng.nextInt(4)) {
                    0 -> assertEquals(reference.takeWord(), view.takeWord())
                    1 -> assertEquals(reference.takeAll(), view.takeAll())
                    2 -> assertEquals(reference.showMore(), view.showMore())
                    else -> {
                        val typed = if (rng.nextBoolean() && reference.shown > 0) {
                            val visible = reference.text.substring(0, reference.shown)
                            visible.substring(0, visible.offsetByCodePoints(0, 1))
                        } else {
                            alphabet.random(rng)
                        }
                        assertEquals(reference.typeThrough(typed), view.typeThrough(typed))
                    }
                }
                assertEquals(reference.text.substring(0, reference.shown), view.visible)
            }
        }
    }

    @Test
    fun takeWordStopsAtTheFirstWordEndAndKeepsTheRest() {
        val view = Suggestion(1, 1, "well-known path", 15, intArrayOf(5, 10, 15), intArrayOf(), 0, 0)
        assertEquals("well-", view.takeWord())
        assertEquals(5, view.wordLength)
        assertEquals("known", view.takeWord())
        assertEquals(" path", view.takeAll())
        assertTrue(view.isEmpty)
        assertNull(view.takeWord())
    }

    @Test
    fun showMoreRevealsBufferedStepsThenAsksTheEngine() {
        val view = Suggestion(1, 1, "one two three", 3, intArrayOf(3, 7, 13), intArrayOf(7, 13), 0, 0)
        assertEquals("one", view.visible)
        assertTrue(view.showMore())
        assertEquals("one two", view.visible)
        assertTrue(view.showMore())
        assertFalse(view.showMore())
    }

    @Test
    fun typingThroughASurrogatePairAdvancesPastBothUnits() {
        val view = Suggestion(1, 1, "😀 ok", 5, intArrayOf(2, 5), intArrayOf(), 0, 0)
        assertFalse(view.typeThrough("x"))
        assertTrue(view.typeThrough("😀"))
        assertEquals(" ok", view.visible)
    }
}
