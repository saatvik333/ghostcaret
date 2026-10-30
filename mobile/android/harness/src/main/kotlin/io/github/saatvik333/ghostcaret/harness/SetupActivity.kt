package io.github.saatvik333.ghostcaret.harness

import android.app.Activity
import android.content.ComponentName
import android.content.Intent
import android.net.Uri
import android.os.Build
import android.os.Bundle
import android.provider.OpenableColumns
import android.provider.Settings
import android.util.TypedValue
import android.view.WindowInsets
import android.view.inputmethod.InputMethodManager
import android.widget.Button
import android.widget.EditText
import android.widget.LinearLayout
import android.widget.ScrollView
import android.widget.TextView
import io.github.saatvik333.ghostcaret.ime.KeyboardService
import io.github.saatvik333.ghostcaret.ime.ModelSettings
import java.io.File

/**
 * The harness's settings screen: enable the keyboard, switch to it, choose a
 * local GGUF model, and try it in a text field.
 */
class SetupActivity : Activity() {
    private lateinit var enabledStatus: TextView
    private lateinit var selectedStatus: TextView
    private lateinit var modelStatus: TextView
    private lateinit var chooseModel: Button
    @Volatile private var copying = false

    private val imeId by lazy { ComponentName(this, KeyboardService::class.java).flattenToShortString() }

    override fun onCreate(savedInstanceState: Bundle?) {
        super.onCreate(savedInstanceState)
        val column = LinearLayout(this).apply {
            orientation = LinearLayout.VERTICAL
            val pad = dp(20f)
            setPadding(pad, pad, pad, pad)
        }
        column.addView(text(R.string.harness_title, 24f))
        column.addView(text(R.string.harness_intro, 16f))

        column.addView(heading(R.string.harness_step_enable))
        enabledStatus = text(0, 16f).also(column::addView)
        column.addView(button(R.string.harness_enable) { startActivity(Intent(Settings.ACTION_INPUT_METHOD_SETTINGS)) })

        column.addView(heading(R.string.harness_step_switch))
        selectedStatus = text(0, 16f).also(column::addView)
        column.addView(button(R.string.harness_switch) {
            getSystemService(InputMethodManager::class.java).showInputMethodPicker()
        })

        column.addView(heading(R.string.harness_step_model))
        modelStatus = text(0, 16f).also(column::addView)
        chooseModel = button(R.string.harness_choose_model) {
            startActivityForResult(
                Intent(Intent.ACTION_OPEN_DOCUMENT).addCategory(Intent.CATEGORY_OPENABLE).setType("*/*"),
                PICK_MODEL,
            )
        }.also(column::addView)

        column.addView(heading(R.string.harness_step_try))
        column.addView(EditText(this).apply {
            setHint(R.string.harness_try_hint)
            minLines = 3
        })

        val scroll = ScrollView(this).apply { addView(column) }
        // Edge to edge from Android 15: keep the content clear of the bars and the keyboard.
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
            scroll.setOnApplyWindowInsetsListener { v, insets ->
                val bars = insets.getInsets(WindowInsets.Type.systemBars() or WindowInsets.Type.ime())
                v.setPadding(bars.left, bars.top, bars.right, bars.bottom)
                insets
            }
        }
        setContentView(scroll)
    }

    override fun onResume() {
        super.onResume()
        refresh()
    }

    override fun onWindowFocusChanged(hasFocus: Boolean) {
        super.onWindowFocusChanged(hasFocus)
        // The input method picker is a dialog that does not pause the activity.
        if (hasFocus) refresh()
    }

    private fun refresh() {
        val imm = getSystemService(InputMethodManager::class.java)
        val enabled = imm.enabledInputMethodList.any { it.id == imeId }
        val selected = Settings.Secure.getString(contentResolver, Settings.Secure.DEFAULT_INPUT_METHOD) == imeId
        enabledStatus.setText(if (enabled) R.string.harness_enabled else R.string.harness_not_enabled)
        selectedStatus.setText(if (selected) R.string.harness_selected else R.string.harness_not_selected)
        if (!copying) {
            val models = ModelSettings.modelsDir(this).listFiles { f -> f.isFile && !f.name.endsWith(".part") }.orEmpty()
            modelStatus.text = if (models.isEmpty()) {
                getString(R.string.harness_no_model)
            } else {
                getString(R.string.harness_models, models.joinToString { "${it.name} (${it.length() / 1_000_000} MB)" })
            }
        }
    }

    @Deprecated("Activity result API without AndroidX")
    override fun onActivityResult(requestCode: Int, resultCode: Int, data: Intent?) {
        super.onActivityResult(requestCode, resultCode, data)
        val uri = data?.data
        if (requestCode == PICK_MODEL && resultCode == RESULT_OK && uri != null) copyModel(uri)
    }

    /** Copies the chosen file into the keyboard's models directory, then selects it. */
    private fun copyModel(uri: Uri) {
        val (name, size) = contentResolver.query(uri, null, null, null, null)?.use { c ->
            c.moveToFirst()
            val name = c.getString(c.getColumnIndexOrThrow(OpenableColumns.DISPLAY_NAME))
            val size = c.getLong(c.getColumnIndexOrThrow(OpenableColumns.SIZE))
            name to size
        } ?: return
        val dir = ModelSettings.modelsDir(this).apply { mkdirs() }
        val target = File(dir, File(name).name)
        val part = File(dir, "${target.name}.part")
        copying = true
        chooseModel.isEnabled = false
        Thread {
            val result = runCatching {
                contentResolver.openInputStream(uri)!!.use { input ->
                    part.outputStream().use { out ->
                        val buffer = ByteArray(1 shl 20)
                        var copied = 0L
                        var shown = -1L
                        while (true) {
                            val n = input.read(buffer)
                            if (n < 0) break
                            out.write(buffer, 0, n)
                            copied += n
                            val percent = if (size > 0) copied * 100 / size else -1
                            if (percent != shown) {
                                shown = percent
                                runOnUiThread { modelStatus.text = getString(R.string.harness_copying, name, percent) }
                            }
                        }
                        out.fd.sync()
                    }
                }
                check(part.renameTo(target)) { "rename failed" }
                ModelSettings.useLocalModel(this, target)?.let { error(it) }
            }
            runOnUiThread {
                copying = false
                chooseModel.isEnabled = true
                refresh()
                result.exceptionOrNull()?.let {
                    part.delete()
                    modelStatus.text = getString(R.string.harness_copy_failed, it.message.orEmpty())
                }
            }
        }.start()
    }

    private fun dp(v: Float) = TypedValue.applyDimension(TypedValue.COMPLEX_UNIT_DIP, v, resources.displayMetrics).toInt()

    private fun text(res: Int, size: Float) = TextView(this).apply {
        if (res != 0) setText(res)
        setTextSize(TypedValue.COMPLEX_UNIT_SP, size)
        setPadding(0, dp(4f), 0, dp(4f))
    }

    private fun heading(res: Int) = text(res, 18f).apply {
        setPadding(0, dp(20f), 0, dp(4f))
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.P) isAccessibilityHeading = true
    }

    private fun button(res: Int, onClick: () -> Unit) = Button(this).apply {
        setText(res)
        minHeight = dp(48f)
        setOnClickListener { onClick() }
    }

    private companion object {
        const val PICK_MODEL = 1
    }
}
