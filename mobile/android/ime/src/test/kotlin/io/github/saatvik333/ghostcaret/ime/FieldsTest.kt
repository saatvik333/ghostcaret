package io.github.saatvik333.ghostcaret.ime

import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Test
import kotlin.random.Random

class FieldsTest {
    private val alphabet = listOf("a", "é", "中", "😀", " ")

    private fun utf8(s: String) = s.toByteArray(Charsets.UTF_8).size

    @Test
    fun byteCountsConvertToTheLongestWholeCodePointRun() {
        val rng = Random(11)
        repeat(5000) {
            val text = buildString { repeat(rng.nextInt(0, 20)) { append(alphabet.random(rng)) } }
            val bytes = rng.nextInt(0, utf8(text) + 3)

            val before = utf16Before(text, bytes)
            val tailStart = text.length - before
            assertTrue(utf8(text.substring(tailStart)) <= bytes)
            if (tailStart > 0) assertTrue(utf8(text.substring(text.offsetByCodePoints(tailStart, -1))) > bytes)

            val after = utf16After(text, bytes)
            assertTrue(utf8(text.substring(0, after)) <= bytes)
            if (after < text.length) assertTrue(utf8(text.substring(0, text.offsetByCodePoints(after, 1))) > bytes)
        }
    }

    @Test
    fun exactByteCountsCoverExactlyThatText() {
        assertEquals(2, utf16Before("ab😀", 4))
        assertEquals(3, utf16Before("ab😀", 5))
        assertEquals(3, utf16After("é中x", 6))
    }

    @Test
    fun splitSurrogatesAreDroppedAtTheEdges() {
        val pair = "😀"
        assertEquals("a", wholeCodePoints(pair.substring(1) + "a"))
        assertEquals("a", wholeCodePoints("a" + pair.substring(0, 1)))
        assertEquals("a😀b", wholeCodePoints("a😀b"))
    }
}
