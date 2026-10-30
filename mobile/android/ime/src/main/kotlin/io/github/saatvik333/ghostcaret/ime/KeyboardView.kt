package io.github.saatvik333.ghostcaret.ime

import android.annotation.SuppressLint
import android.content.Context
import android.graphics.Canvas
import android.graphics.Paint
import android.graphics.Rect
import android.graphics.RectF
import android.media.AudioManager
import android.os.Bundle
import android.os.SystemClock
import android.util.SparseArray
import android.util.TypedValue
import android.view.HapticFeedbackConstants
import android.view.MotionEvent
import android.view.View
import android.view.ViewConfiguration
import android.widget.Button
import androidx.core.view.ViewCompat
import androidx.core.view.accessibility.AccessibilityNodeInfoCompat
import androidx.customview.widget.ExploreByTouchHelper
import java.util.Locale
import kotlin.math.floor

/**
 * The keys, drawn on one canvas. Touch targets cover the whole keyboard with
 * no dead zones: a touch goes to the nearest key in its row. Character keys
 * type on release, so a finger can slide to the right key or into the
 * long-press alternates; a second finger landing releases the first at once.
 * Every key is exposed to TalkBack as a virtual view.
 */
@SuppressLint("ViewConstructor")
internal class KeyboardView(context: Context, private val listener: Listener) : View(context) {
    interface Listener {
        fun onText(text: String)
        fun onBackspace()
        fun onEnter()
        fun onShowInputMethodPicker()
    }

    enum class Shift { OFF, ONCE, LOCKED }

    /** A key at its place: [rect] is its touch area, [body] what is drawn. */
    private class Placed(val def: KeyDef, val rect: RectF, val body: RectF, val row: Int)

    private class Pointer(val id: Int, var key: Placed) {
        var alternates: List<String>? = null
        var selected = 0
    }

    var palette: Palette = Palette.of(context)
        set(value) {
            field = value
            invalidate()
        }

    var page = Page.LETTERS
        set(value) {
            if (field != value) {
                field = value
                relayout()
            }
        }

    /** The key left of space on the letters page. */
    var comma = ","
        set(value) {
            if (field != value) {
                field = value
                relayout()
            }
        }

    var shift = Shift.OFF
        set(value) {
            if (field != value) {
                field = value
                invalidate()
                touchHelper.invalidateRoot()
            }
        }

    var enterLabel = "↵"
        set(value) {
            field = value
            invalidate()
        }
    var enterDescription = ""
    var locale: Locale = Locale.getDefault()
    var soundOn = false

    private val res = context.resources
    private fun dp(v: Float) = TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, v, res.displayMetrics)
    private fun sp(v: Float) = TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_SP, v, res.displayMetrics)

    /** Rows are at least 48 dp tall, the minimum touch target. */
    private val rowHeight = dp(54f)
    private val padding = dp(4f)
    private val gapX = dp(5f)
    private val gapY = dp(8f)
    private val corner = dp(6f)

    private val keyPaint = Paint(Paint.ANTI_ALIAS_FLAG)
    private val label = Paint(Paint.ANTI_ALIAS_FLAG).apply { textAlign = Paint.Align.CENTER }
    private val hint = Paint(Paint.ANTI_ALIAS_FLAG).apply { textAlign = Paint.Align.RIGHT; textSize = sp(11f) }

    private val longPress = ViewConfiguration.getLongPressTimeout().toLong()
    private val repeatStart = ViewConfiguration.getKeyRepeatTimeout().toLong()
    private val repeatEvery = ViewConfiguration.getKeyRepeatDelay().toLong()
    private val doubleTap = ViewConfiguration.getDoubleTapTimeout().toLong()
    private val audio = context.getSystemService(AudioManager::class.java)

    private var keys: List<Placed> = emptyList()
    private val pointers = SparseArray<Pointer>()
    private var lastShiftTap = 0L
    private var accessibilityAlternates: Pair<Placed, List<String>>? = null

    private val touchHelper = Helper()

    init {
        ViewCompat.setAccessibilityDelegate(this, touchHelper)
        isHapticFeedbackEnabled = true
    }

    override fun onMeasure(widthMeasureSpec: Int, heightMeasureSpec: Int) {
        val height = (4 * rowHeight + 2 * padding).toInt()
        setMeasuredDimension(MeasureSpec.getSize(widthMeasureSpec), height)
    }

    override fun onSizeChanged(w: Int, h: Int, oldw: Int, oldh: Int) = relayout()

    private fun relayout() {
        cancelAll()
        val rows = layout(page, comma)
        val unit = (width - 2 * padding) / 10f
        keys = rows.flatMapIndexed { r, row ->
            var x = (width - row.sumOf { it.width.toDouble() }.toFloat() * unit) / 2f
            val top = padding + r * rowHeight
            row.map { def ->
                val rect = RectF(x, top, x + def.width * unit, top + rowHeight)
                x += def.width * unit
                Placed(def, rect, RectF(rect.left + gapX / 2, rect.top + gapY / 2, rect.right - gapX / 2, rect.bottom - gapY / 2), r)
            }
        }
        invalidate()
        touchHelper.invalidateRoot()
    }

    // Drawing

    override fun onDraw(canvas: Canvas) {
        canvas.drawColor(palette.background)
        for (key in keys) drawKey(canvas, key, isPressed(key))
        for (i in 0 until pointers.size()) {
            val p = pointers.valueAt(i)
            p.alternates?.let { drawAlternates(canvas, p.key, it, p.selected) }
        }
        accessibilityAlternates?.let { (key, list) -> drawAlternates(canvas, key, list, -1) }
    }

    private fun keyColor(def: KeyDef) = when (def.type) {
        KeyType.CHAR, KeyType.SPACE -> palette.key
        KeyType.ENTER -> palette.accent
        KeyType.SHIFT -> if (shift == Shift.OFF) palette.functionKey else palette.accent
        else -> palette.functionKey
    }

    private fun drawKey(canvas: Canvas, key: Placed, pressed: Boolean) {
        val body = key.body
        keyPaint.color = if (pressed) palette.pressed else keyColor(key.def)
        canvas.drawRoundRect(body, corner, corner, keyPaint)
        val onAccent = key.def.type == KeyType.ENTER || (key.def.type == KeyType.SHIFT && shift != Shift.OFF)
        label.color = if (onAccent && !pressed) palette.onAccent else palette.text
        val text = labelOf(key.def)
        label.textSize = if (key.def.type == KeyType.CHAR && text.codePointCount(0, text.length) == 1) sp(22f) else sp(16f)
        val baseline = body.centerY() - (label.descent() + label.ascent()) / 2
        canvas.drawText(text, body.centerX(), baseline, label)
        val alternate = key.def.alternates.firstOrNull()
        if (alternate != null && page == Page.LETTERS && key.row == 0) {
            hint.color = palette.faint
            canvas.drawText(alternate, body.right - dp(4f), body.top + hint.textSize, hint)
        }
    }

    private fun alternatesRect(key: Placed, count: Int): RectF {
        val cell = key.rect.width().coerceAtMost(width / 10f * 1.2f)
        val w = cell * count
        val left = (key.rect.left).coerceIn(0f, (width - w).coerceAtLeast(0f))
        val top = (key.rect.top - rowHeight).coerceAtLeast(0f)
        return RectF(left, top, left + w, top + rowHeight)
    }

    private fun drawAlternates(canvas: Canvas, key: Placed, list: List<String>, selected: Int) {
        val panel = alternatesRect(key, list.size)
        val cell = panel.width() / list.size
        keyPaint.color = palette.functionKey
        canvas.drawRoundRect(panel, corner, corner, keyPaint)
        label.textSize = sp(22f)
        list.forEachIndexed { i, s ->
            val cellRect = RectF(panel.left + i * cell, panel.top, panel.left + (i + 1) * cell, panel.bottom)
            if (i == selected) {
                keyPaint.color = palette.accent
                canvas.drawRoundRect(cellRect, corner, corner, keyPaint)
            }
            label.color = if (i == selected) palette.onAccent else palette.text
            val baseline = cellRect.centerY() - (label.descent() + label.ascent()) / 2
            canvas.drawText(s, cellRect.centerX(), baseline, label)
        }
    }

    private fun labelOf(def: KeyDef): String = when (def.type) {
        KeyType.CHAR -> cased(def.text)
        KeyType.SHIFT -> if (shift == Shift.LOCKED) "⇪" else "⇧"
        KeyType.BACKSPACE -> "⌫"
        KeyType.ENTER -> enterLabel
        KeyType.SPACE -> ""
        KeyType.LETTERS -> "ABC"
        KeyType.SYMBOLS -> "?123"
        KeyType.MORE_SYMBOLS -> "=\\<"
    }

    private fun cased(text: String) =
        if (page == Page.LETTERS && shift != Shift.OFF) text.uppercase(locale) else text

    private fun description(def: KeyDef): String = when (def.type) {
        KeyType.CHAR -> cased(def.text)
        KeyType.SHIFT -> res.getString(
            when (shift) {
                Shift.OFF -> R.string.oft_key_shift
                Shift.ONCE -> R.string.oft_key_shift_on
                Shift.LOCKED -> R.string.oft_key_caps_lock
            },
        )
        KeyType.BACKSPACE -> res.getString(R.string.oft_key_delete)
        KeyType.ENTER -> enterDescription
        KeyType.SPACE -> res.getString(R.string.oft_key_space)
        KeyType.LETTERS -> res.getString(R.string.oft_key_letters)
        KeyType.SYMBOLS -> res.getString(R.string.oft_key_symbols)
        KeyType.MORE_SYMBOLS -> res.getString(R.string.oft_key_more_symbols)
    }

    // Touch

    private fun isPressed(key: Placed): Boolean {
        for (i in 0 until pointers.size()) if (pointers.valueAt(i).key === key) return true
        return false
    }

    private fun keyAt(x: Float, y: Float): Placed? {
        if (keys.isEmpty()) return null
        val row = floor((y - padding) / rowHeight).toInt().coerceIn(0, keys.last().row)
        var best: Placed? = null
        var bestDistance = Float.MAX_VALUE
        for (k in keys) {
            if (k.row != row) continue
            val d = if (x < k.rect.left) k.rect.left - x else if (x > k.rect.right) x - k.rect.right else 0f
            if (d < bestDistance) {
                best = k
                bestDistance = d
            }
        }
        return best
    }

    @SuppressLint("ClickableViewAccessibility")
    override fun onTouchEvent(event: MotionEvent): Boolean {
        when (event.actionMasked) {
            MotionEvent.ACTION_DOWN, MotionEvent.ACTION_POINTER_DOWN -> {
                val i = event.actionIndex
                val key = keyAt(event.getX(i), event.getY(i)) ?: return true
                for (j in pointers.size() - 1 downTo 0) {
                    val other = pointers.valueAt(j)
                    if (other.alternates == null && other.key.def.type == KeyType.CHAR) release(other)
                }
                press(Pointer(event.getPointerId(i), key))
            }
            MotionEvent.ACTION_MOVE -> for (i in 0 until event.pointerCount) {
                val p = pointers[event.getPointerId(i)] ?: continue
                val alternates = p.alternates
                if (alternates != null) {
                    val panel = alternatesRect(p.key, alternates.size)
                    val cell = panel.width() / alternates.size
                    val selected = floor((event.getX(i) - panel.left) / cell).toInt().coerceIn(0, alternates.size - 1)
                    if (selected != p.selected) {
                        p.selected = selected
                        invalidate()
                    }
                    continue
                }
                val key = keyAt(event.getX(i), event.getY(i)) ?: continue
                if (key !== p.key && key.def.type == KeyType.CHAR && p.key.def.type == KeyType.CHAR) {
                    removeCallbacks(longPressOf(p))
                    p.key = key
                    scheduleLongPress(p)
                    invalidate()
                }
            }
            MotionEvent.ACTION_UP, MotionEvent.ACTION_POINTER_UP -> {
                pointers[event.getPointerId(event.actionIndex)]?.let(::release)
            }
            MotionEvent.ACTION_CANCEL -> cancelAll()
        }
        return true
    }

    private val timers = HashMap<Int, Runnable>()

    private fun longPressOf(p: Pointer): Runnable = timers.getOrPut(p.id) { Runnable { onLongPress(p) } }

    private fun press(p: Pointer) {
        pointers.put(p.id, p)
        feedback(p.key.def.type)
        if (p.key.def.type == KeyType.BACKSPACE) {
            listener.onBackspace()
            postDelayed(longPressOf(p), repeatStart)
        } else {
            scheduleLongPress(p)
        }
        invalidate()
    }

    private fun scheduleLongPress(p: Pointer) {
        val def = p.key.def
        if (def.alternates.isNotEmpty() || def.type == KeyType.SPACE) postDelayed(longPressOf(p), longPress)
    }

    private fun onLongPress(p: Pointer) {
        if (pointers[p.id] !== p) return
        when (p.key.def.type) {
            KeyType.BACKSPACE -> {
                listener.onBackspace()
                postDelayed(longPressOf(p), repeatEvery)
            }
            KeyType.SPACE -> {
                forget(p)
                listener.onShowInputMethodPicker()
            }
            else -> {
                p.alternates = p.key.def.alternates.map(::cased)
                p.selected = 0
                performHapticFeedback(HapticFeedbackConstants.LONG_PRESS)
                invalidate()
            }
        }
    }

    private fun release(p: Pointer) {
        forget(p)
        val alternates = p.alternates
        if (alternates != null) {
            commitText(alternates[p.selected])
        } else {
            perform(p.key.def)
        }
    }

    private fun forget(p: Pointer) {
        timers.remove(p.id)?.let(::removeCallbacks)
        pointers.remove(p.id)
        invalidate()
    }

    private fun cancelAll() {
        for (r in timers.values) removeCallbacks(r)
        timers.clear()
        pointers.clear()
        accessibilityAlternates = null
        invalidate()
    }

    private fun commitText(text: String) {
        listener.onText(text)
        if (shift == Shift.ONCE && page == Page.LETTERS) shift = Shift.OFF
    }

    private fun perform(def: KeyDef) {
        when (def.type) {
            KeyType.CHAR -> commitText(cased(def.text))
            KeyType.SPACE -> listener.onText(" ")
            KeyType.ENTER -> listener.onEnter()
            KeyType.BACKSPACE -> {}
            KeyType.SHIFT -> {
                val now = SystemClock.uptimeMillis()
                shift = when (shift) {
                    Shift.OFF -> Shift.ONCE
                    Shift.ONCE -> if (now - lastShiftTap <= doubleTap) Shift.LOCKED else Shift.OFF
                    Shift.LOCKED -> Shift.OFF
                }
                lastShiftTap = now
            }
            KeyType.LETTERS -> page = Page.LETTERS
            KeyType.SYMBOLS -> page = Page.SYMBOLS
            KeyType.MORE_SYMBOLS -> page = Page.MORE_SYMBOLS
        }
    }

    private fun feedback(type: KeyType) {
        performHapticFeedback(HapticFeedbackConstants.KEYBOARD_TAP)
        if (!soundOn) return
        audio?.playSoundEffect(
            when (type) {
                KeyType.BACKSPACE -> AudioManager.FX_KEYPRESS_DELETE
                KeyType.SPACE -> AudioManager.FX_KEYPRESS_SPACEBAR
                KeyType.ENTER -> AudioManager.FX_KEYPRESS_RETURN
                else -> AudioManager.FX_KEYPRESS_STANDARD
            },
        )
    }

    // Accessibility

    override fun dispatchHoverEvent(event: MotionEvent): Boolean =
        touchHelper.dispatchHoverEvent(event) || super.dispatchHoverEvent(event)

    /** Virtual views: keys by index, then the open alternates. */
    private inner class Helper : ExploreByTouchHelper(this) {
        private val alternatesBase = 1000

        override fun getVirtualViewAt(x: Float, y: Float): Int {
            accessibilityAlternates?.let { (key, list) ->
                val panel = alternatesRect(key, list.size)
                if (panel.contains(x, y)) {
                    return alternatesBase + floor((x - panel.left) / (panel.width() / list.size)).toInt()
                }
            }
            val key = keyAt(x, y) ?: return INVALID_ID
            return keys.indexOf(key)
        }

        override fun getVisibleVirtualViews(ids: MutableList<Int>) {
            ids.addAll(keys.indices)
            accessibilityAlternates?.let { (_, list) -> ids.addAll(list.indices.map { alternatesBase + it }) }
        }

        // ExploreByTouchHelper requires parent bounds and derives screen bounds from them.
        @Suppress("DEPRECATION")
        override fun onPopulateNodeForVirtualView(id: Int, node: AccessibilityNodeInfoCompat) {
            node.className = Button::class.java.name
            val alternates = accessibilityAlternates
            if (id >= alternatesBase && alternates != null) {
                val (key, list) = alternates
                val panel = alternatesRect(key, list.size)
                val cell = panel.width() / list.size
                val i = (id - alternatesBase).coerceIn(0, list.size - 1)
                node.contentDescription = list[i]
                node.setBoundsInParent(Rect(
                    (panel.left + i * cell).toInt(), panel.top.toInt(),
                    (panel.left + (i + 1) * cell).toInt(), panel.bottom.toInt(),
                ))
                node.addAction(AccessibilityNodeInfoCompat.ACTION_CLICK)
                return
            }
            val key = keys.getOrNull(id)
            if (key == null) {
                node.contentDescription = ""
                node.setBoundsInParent(Rect())
                return
            }
            node.contentDescription = description(key.def)
            val r = key.rect
            node.setBoundsInParent(Rect(r.left.toInt(), r.top.toInt(), r.right.toInt(), r.bottom.toInt()))
            node.addAction(AccessibilityNodeInfoCompat.ACTION_CLICK)
            if (key.def.alternates.isNotEmpty() || key.def.type == KeyType.SPACE) {
                node.addAction(AccessibilityNodeInfoCompat.ACTION_LONG_CLICK)
            }
        }

        override fun onPerformActionForVirtualView(id: Int, action: Int, arguments: Bundle?): Boolean {
            val alternates = accessibilityAlternates
            if (id >= alternatesBase && alternates != null) {
                if (action != AccessibilityNodeInfoCompat.ACTION_CLICK) return false
                accessibilityAlternates = null
                commitText(alternates.second.getOrNull(id - alternatesBase) ?: return false)
                invalidateRoot()
                return true
            }
            val key = keys.getOrNull(id) ?: return false
            when (action) {
                AccessibilityNodeInfoCompat.ACTION_CLICK -> {
                    accessibilityAlternates = null
                    feedback(key.def.type)
                    if (key.def.type == KeyType.BACKSPACE) listener.onBackspace() else perform(key.def)
                }
                AccessibilityNodeInfoCompat.ACTION_LONG_CLICK -> when {
                    key.def.type == KeyType.SPACE -> listener.onShowInputMethodPicker()
                    key.def.alternates.isNotEmpty() -> {
                        accessibilityAlternates = key to key.def.alternates.map(::cased)
                        invalidate()
                        invalidateRoot()
                    }
                    else -> return false
                }
                else -> return false
            }
            return true
        }
    }
}
