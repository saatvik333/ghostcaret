package io.github.saatvik333.ghostcaret.ime

import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Typeface
import android.text.SpannableStringBuilder
import android.text.Spanned
import android.text.TextUtils
import android.text.style.ForegroundColorSpan
import android.text.style.StyleSpan
import android.util.TypedValue
import android.view.GestureDetector
import android.view.Gravity
import android.view.MotionEvent
import android.widget.LinearLayout
import android.widget.TextView
import androidx.core.view.ViewCompat
import androidx.core.view.accessibility.AccessibilityNodeInfoCompat.AccessibilityActionCompat
import kotlin.math.abs

/**
 * The strip above the keys. With a suggestion: tap takes the next word (shown
 * in full strength, the rest faint), long-press or "All" takes everything,
 * "»" shows more, and a horizontal swipe cycles alternatives. Without one it
 * shows why suggestions are not available, if anything blocks them.
 */
@SuppressLint("ViewConstructor")
internal class StripView(context: Context, private val listener: Listener) : LinearLayout(context) {
    interface Listener {
        fun onStripAction(action: Int)
    }

    private val res = context.resources
    private fun dp(v: Float) = TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, v, res.displayMetrics).toInt()

    private var palette = Palette.of(context)

    private val draft = button(R.string.oft_strip_reply, R.string.oft_strip_reply_description) {
        listener.onStripAction(Codes.Action.FLOW_DRAFT)
    }

    /** Reply & rewrite rewrites when text is selected and drafts a reply otherwise. */
    var rewrite = false
        set(value) {
            if (field == value) return
            field = value
            draft.setText(if (value) R.string.oft_strip_rewrite else R.string.oft_strip_reply)
            draft.contentDescription =
                res.getString(if (value) R.string.oft_strip_rewrite_description else R.string.oft_strip_reply_description)
        }
    private val text = TextView(context).apply {
        layoutParams = LayoutParams(0, LayoutParams.MATCH_PARENT, 1f)
        gravity = Gravity.CENTER_VERTICAL
        setPadding(dp(12f), 0, dp(12f), 0)
        setTextSize(TypedValue.COMPLEX_UNIT_SP, 16f)
        maxLines = 2
        ellipsize = TextUtils.TruncateAt.END
    }
    private val alternative = TextView(context).apply {
        layoutParams = LayoutParams(LayoutParams.WRAP_CONTENT, LayoutParams.MATCH_PARENT)
        gravity = Gravity.CENTER_VERTICAL
        setPadding(dp(4f), 0, dp(4f), 0)
        setTextSize(TypedValue.COMPLEX_UNIT_SP, 12f)
        importantForAccessibility = IMPORTANT_FOR_ACCESSIBILITY_NO
    }
    private val all = button(R.string.oft_strip_all, R.string.oft_strip_all_description) {
        listener.onStripAction(Codes.Action.TAKE_ALL)
    }
    private val more = button(R.string.oft_strip_more, R.string.oft_strip_more_description) {
        listener.onStripAction(Codes.Action.SHOW_MORE)
    }

    private var showing: Suggestion? = null

    /** The rest of the suggestion in full strength instead of faint. */
    var highContrast = false

    /** Announce each suggestion to accessibility services as it appears. */
    var announce = false
        set(value) {
            field = value
            text.accessibilityLiveRegion = if (value) ACCESSIBILITY_LIVE_REGION_POLITE else ACCESSIBILITY_LIVE_REGION_NONE
        }

    init {
        orientation = HORIZONTAL
        minimumHeight = dp(48f)
        addView(draft)
        addView(text)
        addView(alternative)
        addView(all)
        addView(more)
        val swipe = GestureDetector(context, object : GestureDetector.SimpleOnGestureListener() {
            override fun onDown(e: MotionEvent) = true

            override fun onSingleTapUp(e: MotionEvent): Boolean {
                if (showing == null) return false
                listener.onStripAction(Codes.Action.TAKE_WORD)
                return true
            }

            override fun onLongPress(e: MotionEvent) {
                if (showing != null) listener.onStripAction(Codes.Action.TAKE_ALL)
            }

            override fun onFling(e1: MotionEvent?, e2: MotionEvent, vx: Float, vy: Float): Boolean {
                val s = showing ?: return false
                if (s.alternatives < 2 || abs(vx) < abs(vy)) return false
                listener.onStripAction(if (vx < 0) Codes.Action.NEXT_ALTERNATIVE else Codes.Action.PREV_ALTERNATIVE)
                return true
            }
        })
        @SuppressLint("ClickableViewAccessibility")
        text.setOnTouchListener { _, e -> swipe.onTouchEvent(e) }
        applyPalette(palette)
        show(null, null)
    }

    fun applyPalette(p: Palette) {
        palette = p
        setBackgroundColor(p.background)
        text.setTextColor(p.text)
        alternative.setTextColor(p.faint)
        for (b in listOf(draft, all, more)) b.setTextColor(p.accent)
        showing?.let { show(it, null) }
    }

    /** Shows [suggestion], or [status] (which may be null) when there is none. */
    fun show(suggestion: Suggestion?, status: String?) {
        showing = suggestion
        val visible = if (suggestion != null) VISIBLE else GONE
        all.visibility = visible
        more.visibility = visible
        if (suggestion == null) {
            text.text = status.orEmpty()
            text.setTextColor(palette.faint)
            text.contentDescription = null
            alternative.visibility = GONE
            ViewCompat.setAccessibilityDelegate(text, null)
            text.isClickable = false
            return
        }
        val shown = suggestion.visible
        val word = suggestion.wordLength
        val styled = SpannableStringBuilder(shown).apply {
            setSpan(StyleSpan(Typeface.BOLD), 0, word, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            if (!highContrast) {
                setSpan(ForegroundColorSpan(palette.faint), word, shown.length, Spanned.SPAN_EXCLUSIVE_EXCLUSIVE)
            }
        }
        text.setTextColor(palette.text)
        text.text = styled
        text.contentDescription = res.getString(R.string.oft_strip_suggestion_description, shown)
        text.isClickable = true
        if (suggestion.alternatives > 1) {
            alternative.text = res.getString(R.string.oft_strip_alternative, suggestion.alternative, suggestion.alternatives)
            alternative.visibility = VISIBLE
        } else {
            alternative.visibility = GONE
        }
        describeActions(suggestion)
    }

    /** TalkBack actions on the suggestion, mirroring the touch gestures. */
    private fun describeActions(s: Suggestion) {
        for (id in actionIds) ViewCompat.removeAccessibilityAction(text, id)
        actionIds.clear()
        fun add(label: Int, action: Int) {
            actionIds += ViewCompat.addAccessibilityAction(text, res.getString(label)) { _, _ ->
                listener.onStripAction(action)
                true
            }
        }
        ViewCompat.replaceAccessibilityAction(
            text,
            AccessibilityActionCompat.ACTION_CLICK,
            res.getString(R.string.oft_strip_take_word),
        ) { _, _ ->
            listener.onStripAction(Codes.Action.TAKE_WORD)
            true
        }
        add(R.string.oft_strip_all_description, Codes.Action.TAKE_ALL)
        add(R.string.oft_strip_more_description, Codes.Action.SHOW_MORE)
        if (s.alternatives > 1) {
            add(R.string.oft_strip_next_alternative, Codes.Action.NEXT_ALTERNATIVE)
            add(R.string.oft_strip_prev_alternative, Codes.Action.PREV_ALTERNATIVE)
        }
    }

    private val actionIds = ArrayList<Int>()

    private fun button(label: Int, description: Int, onClick: () -> Unit) = TextView(context).apply {
        layoutParams = LayoutParams(LayoutParams.WRAP_CONTENT, LayoutParams.MATCH_PARENT)
        minWidth = dp(48f)
        gravity = Gravity.CENTER
        setPadding(dp(8f), 0, dp(8f), 0)
        setTextSize(TypedValue.COMPLEX_UNIT_SP, 15f)
        setText(label)
        contentDescription = res.getString(description)
        isClickable = true
        isFocusable = true
        setOnClickListener { onClick() }
        background = with(TypedValue()) {
            context.theme.resolveAttribute(android.R.attr.selectableItemBackground, this, true)
            context.getDrawable(resourceId)
        }
    }
}
