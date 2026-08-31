package com.nomo.desktop

import java.net.URLEncoder

/** Keep shared text (including URLs) as text before tao tries to parse it as a URL. */
internal fun sharedTextDataUrl(text: String, deliveryId: String): String =
  "data:text/plain," + URLEncoder.encode(text, "UTF-8").replace("+", "%20") +
    "#nomo-share=" + deliveryId
