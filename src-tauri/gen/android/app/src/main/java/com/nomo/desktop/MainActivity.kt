package com.nomo.desktop

import android.os.Bundle
import android.content.Intent
import androidx.activity.enableEdgeToEdge
import java.util.UUID

class MainActivity : TauriActivity() {
  override fun onCreate(savedInstanceState: Bundle?) {
    enableEdgeToEdge()
    normalizeSharedText(intent)
    super.onCreate(savedInstanceState)
  }

  override fun onNewIntent(intent: Intent) {
    normalizeSharedText(intent)
    super.onNewIntent(intent)
    setIntent(intent)
  }

  private fun normalizeSharedText(intent: Intent?) {
    if (intent?.action != Intent.ACTION_SEND || intent.type != "text/plain" ||
      intent.hasExtra(Intent.EXTRA_STREAM) || intent.getBooleanExtra("nomo.text.normalized", false)) return
    val text = intent.getCharSequenceExtra(Intent.EXTRA_TEXT)?.toString().orEmpty()
    intent.putExtra(Intent.EXTRA_TEXT, sharedTextDataUrl(text, UUID.randomUUID().toString()))
    // Activity recreation must not encode an already-normalized delivery a second time.
    intent.putExtra("nomo.text.normalized", true)
  }
}
