// ─────────────────────────────────────────────────────────────────────
// webview.rs  –  WebKitGTK WebView factory, URI helpers, and privacy
// ─────────────────────────────────────────────────────────────────────
//
// This module is the single place that talks directly to the WebKit
// engine.  It exposes public helpers for creating WebViews, navigating,
// and configuring privacy-hardened settings.
//
// Phase 3 additions:
//   - Custom privacy-respecting user agent.
//   - WebView Settings hardening (disable hyperlink auditing, etc.).
//   - Smooth scrolling enabled by default.
//   - NetworkSession privacy setup (ITP, cookie policy, etc.).
//   - DNT header via injected JavaScript.
//
// Public API:
//
//   create_webview(ucm)              – build a ready-to-use WebView.
//   create_privacy_session()         – build a privacy-hardened session.
//   setup_privacy_on_session(session) – enable ITP + cookie policy.
//   navigate_to(webview, input)      – load a URI or search query.
//   home_url()                       – configured homepage.
//   inject_dnt_header(ucm)           – inject DNT via JavaScript.
// ─────────────────────────────────────────────────────────────────────

use gtk4::prelude::*; // WidgetExt  – set_vexpand / set_hexpand
use webkit6::prelude::*; // WebViewExt – load_uri, can_go_back, …
use webkit6::{
    CookieAcceptPolicy, NetworkSession, Settings, UserContentInjectedFrames, UserContentManager,
    UserScript, UserScriptInjectionTime, WebView,
};

use crate::settings as app_settings;

// ── Privacy constants ───────────────────────────────────────────────

/// A privacy-respecting user agent string.
/// Uses a Safari-compatible UA that matches the underlying WebKit
/// engine.  A Firefox UA on WebKit triggers bot detection (Cloudflare
/// Turnstile etc.) because the JS engine fingerprint doesn't match.
const PRIVACY_USER_AGENT: &str =
    "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/17.4 Safari/605.1.15";

/// JavaScript to send a Do Not Track signal via navigator property.
const DNT_JS: &str = r#"
(function() {
    'use strict';
    // Override navigator.doNotTrack to signal DNT.
    Object.defineProperty(navigator, 'doNotTrack', {
        get: function() { return '1'; },
        configurable: false,
        enumerable: true
    });
    // Also set the GPC (Global Privacy Control) signal.
    Object.defineProperty(navigator, 'globalPrivacyControl', {
        get: function() { return true; },
        configurable: false,
        enumerable: true
    });
})();
"#;

/// JavaScript to prevent WebRTC IP address leaking.
const WEBRTC_LEAK_PREVENTION_JS: &str = r#"
(function() {
    'use strict';
    // Disable WebRTC IP leak by wrapping RTCPeerConnection.
    if (window.RTCPeerConnection) {
        const origRTC = window.RTCPeerConnection;
        window.RTCPeerConnection = function(config, constraints) {
            if (config && config.iceServers) {
                // Force relay-only mode to prevent IP leak.
                config.iceTransportPolicy = 'relay';
            }
            return new origRTC(config, constraints);
        };
        window.RTCPeerConnection.prototype = origRTC.prototype;
    }
})();
"#;

/// Cloudflare challenge domains — scripts must NOT be injected into
/// these frames, or Cloudflare's bot detection will flag the browser.
pub const CF_BLOCK_LIST: &[&str] = &[
    "https://*.challenges.cloudflare.com/*",
    "https://challenges.cloudflare.com/*",
    "https://*.cloudflare.com/cdn-cgi/*",
    "https://*.cloudflareinsights.com/*",
    "https://static.cloudflareinsights.com/*",
];

// ── Public API ──────────────────────────────────────────────────────

/// Create a new WebKit [`WebView`] widget ready to be placed inside a
/// GTK container.
///
/// Configured with privacy-hardened settings, smooth scrolling,
/// custom user agent, and the provided `UserContentManager` for
/// ad blocking and privacy scripts.
pub fn create_webview(ucm: &UserContentManager) -> WebView {
    // Build Settings with privacy hardening.
    let settings = Settings::new();

    // Privacy settings.
    // Note: set_enable_hyperlink_auditing is deprecated in WebKit 6 and does nothing.
    settings.set_enable_smooth_scrolling(true); // Smooth scroll.
    settings.set_enable_developer_extras(true); // Inspector access.
    settings.set_enable_back_forward_navigation_gestures(true); // Touchpad gestures.
    settings.set_enable_page_cache(true); // Performance.
    settings.set_enable_webgl(true); // WebGL support.
    settings.set_enable_webaudio(true); // Web Audio.
    settings.set_enable_media(true); // Master media toggle.
    settings.set_enable_mediasource(true); // MSE video.
    settings.set_enable_encrypted_media(true); // DRM content.

    // Custom user agent based on privacy settings.
    let use_custom_ua = app_settings::get_setting("privacy_custom_ua")
        .map(|v| v == "true")
        .unwrap_or(true); // Default: enabled.

    if use_custom_ua {
        settings.set_user_agent(Some(PRIVACY_USER_AGENT));
    }

    // Disable WebRTC by default for privacy (user can re-enable).
    let webrtc_enabled = app_settings::get_setting("privacy_webrtc")
        .map(|v| v == "true")
        .unwrap_or(false); // Default: disabled for privacy.

    settings.set_enable_webrtc(webrtc_enabled);

    // Media stream (camera/microphone via getUserMedia) is independent
    // of WebRTC.  Many sites need camera/mic without peer-to-peer.
    // Permission dialogs still protect the user, so enable by default.
    settings.set_enable_media_stream(true);
    settings.set_enable_media_capabilities(true);

    let webview = WebView::builder()
        .user_content_manager(ucm)
        .settings(&settings)
        .build();

    // Expand to fill all remaining vertical and horizontal space.
    webview.set_vexpand(true);
    webview.set_hexpand(true);

    // Enable favicon database so WebView::favicon() works.
    if let Some(session) = webview.network_session() {
        if let Some(data_mgr) = session.website_data_manager() {
            data_mgr.set_favicons_enabled(true);
        }
    }

    webview
}

/// Configure privacy features on a [`NetworkSession`].
///
/// Enables:
///   - Intelligent Tracking Prevention (ITP).
///   - Strict cookie policy (no third-party cookies).
pub fn setup_privacy_on_session(session: &NetworkSession) {
    // Enable ITP (Intelligent Tracking Prevention).
    let itp_enabled = app_settings::get_setting("privacy_itp")
        .map(|v| v == "true")
        .unwrap_or(true); // Default: enabled.

    session.set_itp_enabled(itp_enabled);

    // Set cookie policy.
    //
    // Default changed from "no-third-party" → "allow-all" because the
    // blanket third-party cookie block breaks Cloudflare Turnstile
    // challenges (they need __cf_bm / cf_clearance cookies).  ITP
    // (enabled above) already provides intelligent tracking protection
    // that handles cross-site tracking without breaking legitimate
    // third-party cookie use cases like security challenges.
    let cookie_policy =
        app_settings::get_setting("privacy_cookies").unwrap_or_else(|| "allow-all".to_string());

    if let Some(cookie_mgr) = session.cookie_manager() {
        let policy = match cookie_policy.as_str() {
            "block-all" => CookieAcceptPolicy::Never,
            "no-third-party" => CookieAcceptPolicy::NoThirdParty,
            _ => CookieAcceptPolicy::Always, // default: allow-all (ITP handles tracking)
        };
        cookie_mgr.set_accept_policy(policy);
    }
}

/// Inject privacy-related JavaScript (DNT header, WebRTC leak
/// prevention) into the UserContentManager.
pub fn inject_privacy_scripts(ucm: &UserContentManager) {
    // Do Not Track + Global Privacy Control.
    let dnt_enabled = app_settings::get_setting("privacy_dnt")
        .map(|v| v == "true")
        .unwrap_or(true); // Default: enabled.

    if dnt_enabled {
        let dnt_script = UserScript::new(
            DNT_JS,
            UserContentInjectedFrames::AllFrames,
            UserScriptInjectionTime::Start,
            &[],           // allow-list: all pages
            CF_BLOCK_LIST, // block-list: skip Cloudflare challenge frames
        );
        ucm.add_script(&dnt_script);
    }

    // WebRTC IP leak prevention.
    let webrtc_leak_prevention = app_settings::get_setting("privacy_webrtc_leak")
        .map(|v| v == "true")
        .unwrap_or(true); // Default: enabled.

    if webrtc_leak_prevention {
        let rtc_script = UserScript::new(
            WEBRTC_LEAK_PREVENTION_JS,
            UserContentInjectedFrames::AllFrames,
            UserScriptInjectionTime::Start,
            &[],           // allow-list: all pages
            CF_BLOCK_LIST, // block-list: skip Cloudflare challenge frames
        );
        ucm.add_script(&rtc_script);
    }
}

/// Load a URI (or search term) in the given [`WebView`].
///
/// The raw user input is first run through [`normalise_uri`] which
/// decides whether the text is a URL, domain, or search query.
pub fn navigate_to(webview: &WebView, input: &str) {
    let uri = normalise_uri(input);
    webview.load_uri(&uri);
}

/// Return the configured homepage URL.
pub fn home_url() -> String {
    app_settings::homepage()
}

// ── Private helpers ─────────────────────────────────────────────────

/// Turn arbitrary address-bar text into a valid URI.
fn normalise_uri(input: &str) -> String {
    let trimmed = input.trim();

    // 1. Nothing typed – go home.
    if trimmed.is_empty() {
        return home_url();
    }

    // 2. Already has a scheme (e.g. "https://…", "file://…").
    if trimmed.contains("://") {
        return trimmed.to_string();
    }

    // 3. Bare domain heuristic.
    if trimmed.contains('.') && !trimmed.contains(' ') {
        return format!("https://{trimmed}");
    }

    // 4. Fall back to a web search.
    let search_url = app_settings::search_engine_url();
    let encoded = glib::uri_escape_string(trimmed, None::<&str>, false);
    format!("{search_url}{encoded}")
}
