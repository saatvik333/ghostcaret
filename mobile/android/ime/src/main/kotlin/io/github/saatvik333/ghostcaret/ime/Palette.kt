package io.github.saatvik333.ghostcaret.ime

import android.content.Context
import android.content.res.Configuration
import android.graphics.Color
import android.os.Build

/**
 * Keyboard colors: the system's dynamic palette on Android 12 and later, a
 * neutral one before; light or dark following the system.
 */
internal class Palette(
    val background: Int,
    val key: Int,
    val functionKey: Int,
    val pressed: Int,
    val text: Int,
    val faint: Int,
    val accent: Int,
    val onAccent: Int,
) {
    companion object {
        fun of(context: Context): Palette {
            val dark = context.resources.configuration.uiMode and Configuration.UI_MODE_NIGHT_MASK ==
                Configuration.UI_MODE_NIGHT_YES
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S) {
                val c = { id: Int -> context.getColor(id) }
                return if (dark) {
                    Palette(
                        background = c(android.R.color.system_neutral1_900),
                        key = c(android.R.color.system_neutral1_700),
                        functionKey = c(android.R.color.system_neutral2_800),
                        pressed = c(android.R.color.system_neutral1_500),
                        text = c(android.R.color.system_neutral1_50),
                        faint = c(android.R.color.system_neutral1_300),
                        accent = c(android.R.color.system_accent1_200),
                        onAccent = c(android.R.color.system_accent1_800),
                    )
                } else {
                    Palette(
                        background = c(android.R.color.system_neutral1_100),
                        key = c(android.R.color.system_neutral1_10),
                        functionKey = c(android.R.color.system_neutral2_200),
                        pressed = c(android.R.color.system_neutral1_300),
                        text = c(android.R.color.system_neutral1_900),
                        faint = c(android.R.color.system_neutral1_600),
                        accent = c(android.R.color.system_accent1_600),
                        onAccent = c(android.R.color.system_accent1_0),
                    )
                }
            }
            return if (dark) {
                Palette(
                    background = Color.rgb(0x1B, 0x1B, 0x1F), key = Color.rgb(0x46, 0x46, 0x4A),
                    functionKey = Color.rgb(0x30, 0x30, 0x34), pressed = Color.rgb(0x76, 0x76, 0x7A),
                    text = Color.rgb(0xF1, 0xF0, 0xF4), faint = Color.rgb(0xC7, 0xC6, 0xCA),
                    accent = Color.rgb(0xA8, 0xC7, 0xFA), onAccent = Color.rgb(0x06, 0x2E, 0x6F),
                )
            } else {
                Palette(
                    background = Color.rgb(0xE9, 0xEA, 0xEE), key = Color.WHITE,
                    functionKey = Color.rgb(0xC9, 0xCC, 0xD3), pressed = Color.rgb(0xAB, 0xAD, 0xB3),
                    text = Color.rgb(0x1B, 0x1B, 0x1F), faint = Color.rgb(0x5E, 0x5E, 0x62),
                    accent = Color.rgb(0x0B, 0x57, 0xD0), onAccent = Color.WHITE,
                )
            }
        }
    }
}
