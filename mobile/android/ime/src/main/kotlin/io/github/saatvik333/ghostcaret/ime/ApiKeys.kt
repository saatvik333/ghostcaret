package io.github.saatvik333.ghostcaret.ime

import android.content.Context
import android.security.keystore.KeyGenParameterSpec
import android.security.keystore.KeyProperties
import java.io.File
import java.security.KeyStore
import javax.crypto.Cipher
import javax.crypto.KeyGenerator
import javax.crypto.SecretKey
import javax.crypto.spec.GCMParameterSpec

/**
 * Provider API keys, encrypted with an AES key that never leaves the Android
 * Keystore. One file per provider id in the no-backup directory, so keys are
 * not copied off the device by backups. Keys are never logged.
 */
class ApiKeys(context: Context) {
    private val dir = File(context.noBackupFilesDir, "api-keys")

    fun save(provider: String, key: String) {
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.ENCRYPT_MODE, secretKey())
        val sealed = cipher.iv + cipher.doFinal(key.toByteArray(Charsets.UTF_8))
        dir.mkdirs()
        val temp = File(dir, "${name(provider)}.tmp")
        temp.outputStream().use { out ->
            out.write(sealed)
            out.fd.sync()
        }
        check(temp.renameTo(file(provider))) { "cannot store the key" }
    }

    fun load(provider: String): String? {
        val sealed = file(provider).takeIf(File::exists)?.readBytes() ?: return null
        if (sealed.size <= IV_BYTES) return null
        val cipher = Cipher.getInstance(TRANSFORMATION)
        cipher.init(Cipher.DECRYPT_MODE, secretKey(), GCMParameterSpec(TAG_BITS, sealed, 0, IV_BYTES))
        return String(cipher.doFinal(sealed, IV_BYTES, sealed.size - IV_BYTES), Charsets.UTF_8)
    }

    fun delete(provider: String) {
        file(provider).delete()
    }

    /** File names from the hex of the id, so any id is a safe name. */
    private fun name(provider: String) = provider.toByteArray(Charsets.UTF_8).joinToString("") { "%02x".format(it) }

    private fun file(provider: String) = File(dir, name(provider))

    private fun secretKey(): SecretKey {
        val store = KeyStore.getInstance(KEYSTORE).apply { load(null) }
        (store.getKey(ALIAS, null) as? SecretKey)?.let { return it }
        val generator = KeyGenerator.getInstance(KeyProperties.KEY_ALGORITHM_AES, KEYSTORE)
        generator.init(
            KeyGenParameterSpec.Builder(ALIAS, KeyProperties.PURPOSE_ENCRYPT or KeyProperties.PURPOSE_DECRYPT)
                .setBlockModes(KeyProperties.BLOCK_MODE_GCM)
                .setEncryptionPaddings(KeyProperties.ENCRYPTION_PADDING_NONE)
                .setKeySize(256)
                .build(),
        )
        return generator.generateKey()
    }

    private companion object {
        const val KEYSTORE = "AndroidKeyStore"
        const val ALIAS = "ghostcaret-api-keys"
        const val TRANSFORMATION = "AES/GCM/NoPadding"
        const val IV_BYTES = 12
        const val TAG_BITS = 128
    }
}
