package io.github.saatvik333.ghostcaret.ime

import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Typeface
import android.graphics.drawable.GradientDrawable
import android.text.TextUtils
import android.util.TypedValue
import android.view.Gravity
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView

/** A Reply & rewrite card as the engine describes it. */
internal class Card(
    val field: Long,
    val title: String,
    val items: Array<String>,
    /** Index of the highlighted item, or -1. */
    val selected: Int,
    /** The custom direction being typed, or null. */
    val input: String?,
    val preview: String?,
    val status: String?,
    val hint: String,
)

/**
 * The Reply & rewrite card, in the keyboard area above the keys. Items are picked
 * by touch or by number; letters typed on the keys go to the card. "Insert"
 * and "Close" stand for Enter and Esc. The engine's key hints are shown only
 * when a hardware keyboard is attached, since they name its keys.
 */
@SuppressLint("ViewConstructor")
internal class DraftCardView(context: Context, private val listener: Listener) : LinearLayout(context) {
    interface Listener {
        fun onCardKey(key: Int, value: Int)
    }

    private val res = context.resources
    private fun dp(v: Float) = TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, v, res.displayMetrics).toInt()

    private var palette = Palette.of(context)

    private val title = text(16f).apply {
        layoutParams = LayoutParams(0, LayoutParams.WRAP_CONTENT, 1f)
        setTypeface(typeface, Typeface.BOLD)
    }
    private val close = action(R.string.oft_card_close) { listener.onCardKey(Codes.CardKey.ESCAPE, 0) }
    private val insert = action(R.string.oft_card_insert) { listener.onCardKey(Codes.CardKey.ENTER, 0) }
    private val items = LinearLayout(context).apply { orientation = VERTICAL }
    private val input = text(16f)
    private val preview = text(16f)
    /** Scrolls past a fifth of the screen, so the card never pushes the keys off it. */
    private val previewScroll = object : ScrollView(context) {
        override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
            val max = MeasureSpec.makeMeasureSpec(res.displayMetrics.heightPixels / 5, MeasureSpec.AT_MOST)
            super.onMeasure(widthMeasureSpec, max)
        }
    }.apply { addView(preview) }
    private val status = text(14f).apply { accessibilityLiveRegion = ACCESSIBILITY_LIVE_REGION_POLITE }
    private val hint = text(12f)

    init {
        orientation = VERTICAL
        setPadding(dp(12f), dp(8f), dp(12f), dp(8f))
        addView(LinearLayout(context).apply {
            orientation = HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            addView(title)
            addView(close)
        })
        addView(items)
        addView(input)
        addView(previewScroll, LayoutParams(LayoutParams.MATCH_PARENT, LayoutParams.WRAP_CONTENT))
        addView(status)
        addView(LinearLayout(context).apply {
            orientation = HORIZONTAL
            gravity = Gravity.CENTER_VERTICAL
            addView(hint, LayoutParams(0, LayoutParams.WRAP_CONTENT, 1f))
            addView(insert)
        })
    }

    fun applyPalette(p: Palette) {
        palette = p
        setBackgroundColor(p.background)
        for (t in listOf(title, input, preview)) t.setTextColor(p.text)
        for (t in listOf(status, hint)) t.setTextColor(p.faint)
        close.setTextColor(p.accent)
        insert.setTextColor(p.onAccent)
        insert.background = GradientDrawable().apply {
            cornerRadius = dp(18f).toFloat()
            setColor(p.accent)
        }
    }

    fun show(card: Card, hardwareKeyboard: Boolean) {
        title.text = card.title
        items.removeAllViews()
        card.items.forEachIndexed { i, item ->
            items.addView(text(16f).apply {
                text = res.getString(R.string.oft_card_item, i + 1, item)
                setTextColor(palette.text)
                minHeight = dp(48f)
                gravity = Gravity.CENTER_VERTICAL
                maxLines = 2
                ellipsize = TextUtils.TruncateAt.END
                isClickable = true
                isFocusable = true
                isSelected = i == card.selected
                if (i == card.selected) {
                    background = GradientDrawable().apply {
                        cornerRadius = dp(8f).toFloat()
                        setColor(palette.key)
                    }
                }
                setOnClickListener { listener.onCardKey(Codes.CardKey.PICK, i + 1) }
            })
        }
        input.visibility = if (card.input != null) VISIBLE else GONE
        input.text = res.getString(R.string.oft_card_input, card.input.orEmpty())
        preview.text = card.preview.orEmpty()
        previewScroll.visibility = if (card.preview != null) VISIBLE else GONE
        status.text = card.status.orEmpty()
        status.visibility = if (card.status.isNullOrEmpty()) GONE else VISIBLE
        hint.text = if (hardwareKeyboard) card.hint else ""
        insert.visibility = if (card.preview != null) VISIBLE else GONE
        requestLayout()
    }

    private fun text(size: Float) = TextView(context).apply {
        setTextSize(TypedValue.COMPLEX_UNIT_SP, size)
        setPadding(dp(4f), dp(4f), dp(4f), dp(4f))
    }

    private fun action(label: Int, onClick: () -> Unit) = TextView(context).apply {
        setText(label)
        setTextSize(TypedValue.COMPLEX_UNIT_SP, 15f)
        minWidth = dp(48f)
        minHeight = dp(48f)
        gravity = Gravity.CENTER
        setPadding(dp(16f), 0, dp(16f), 0)
        isClickable = true
        isFocusable = true
        setOnClickListener { onClick() }
    }
}
