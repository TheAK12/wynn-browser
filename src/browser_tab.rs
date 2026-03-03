// ─────────────────────────────────────────────────────────────────────
// browser_tab.rs  –  Tab creation and active-tab helpers
// ─────────────────────────────────────────────────────────────────────
//
// Manages the relationship between `adw::TabView` pages and their
// underlying WebKit `WebView` widgets.  In Phase 3, the TabView is
// used internally for tab management while the visible UI is a
// vertical sidebar ListBox (managed by window.rs).
//
// Public API:
//
//   add_tab()        – create a new WebView, append it to the TabView,
//                      wire per-tab signals.  Returns the TabPage.
//   active_webview() – return the WebView of the currently selected tab.
//   update_security_icon() – update HTTPS lock indicator.
// ─────────────────────────────────────────────────────────────────────

use adw::prelude::*;
use glib::clone;
use glib::prelude::Cast;
use gtk4::gdk::prelude::TextureExt;
use gtk4::prelude::*;
use libadwaita as adw;
use webkit6::prelude::*;
use webkit6::{LoadEvent, PermissionState, ScriptDialogType, UserContentManager, WebView};

use crate::history;
use crate::passwords;
use crate::webview;

// ── Password capture via JavaScript ─────────────────────────────────
//
// Instead of using WebKit's connect_submit_form signal (which calls the
// C function webkit_form_submission_request_list_text_fields and
// segfaults on JS-driven forms like Google login), we inject JavaScript
// that intercepts form submissions containing password fields.
//
// The JS extracts the username and password from the DOM and sends them
// back to Rust via WebKit's message handler system
// (window.webkit.messageHandlers.wynnPasswordCapture.postMessage).
//
// The message handler is registered once on the shared UserContentManager
// via register_password_capture_handler() called from window.rs.

/// JavaScript that captures credentials from form submissions.
///
/// Hooks into the 'submit' event on all forms containing a password
/// input.  Also watches for click events on submit buttons (for sites
/// that submit via JS rather than native form submission).
///
/// Google-specific handling: Google's login splits username and password
/// across separate pages.  We stash the email entered on the identifier
/// page into `sessionStorage` and retrieve it on the password page.
/// We also match `[role="button"]` and `[jsaction]` elements that Google
/// uses instead of standard `<button>` tags.
const PASSWORD_CAPTURE_JS: &str = r#"(function() {
    'use strict';
    if (window.__wynnPwCapture) return;
    window.__wynnPwCapture = true;

    var STORAGE_KEY = '__wynnCapturedUser';

    // ── Utility: find the best username on the current page ───────
    function findUsername(scope) {
        var sel = 'input[type="text"], input[type="email"], input[type="tel"], input:not([type])';
        var inputs = (scope || document).querySelectorAll(sel);
        for (var i = 0; i < inputs.length; i++) {
            var inp = inputs[i];
            // Skip hidden / off-screen inputs.
            if (inp.value && inp.offsetParent !== null) {
                return inp.value;
            }
        }
        return '';
    }

    // ── Utility: find the password value on the current page ──────
    function findPassword(scope) {
        var pwFields = (scope || document).querySelectorAll('input[type="password"]');
        var pw = '';
        for (var i = 0; i < pwFields.length; i++) {
            if (pwFields[i].value) pw = pwFields[i].value;
        }
        return pw;
    }

    // ── Utility: send credentials to Rust backend ────────────────
    // Deduplication: only send once per unique (user, pw) combo
    // within a short window.  Multiple events (submit, click, keydown)
    // often fire for the same form action.
    var _lastSent = '';
    var _lastSentTime = 0;

    function sendCreds(user, pw) {
        if (!pw) return;
        var key = (user || '') + '\x00' + pw;
        var now = Date.now();
        // Suppress duplicate within 3 seconds.
        if (key === _lastSent && (now - _lastSentTime) < 3000) return;
        _lastSent = key;
        _lastSentTime = now;
        try {
            window.webkit.messageHandlers.wynnPasswordCapture.postMessage(
                JSON.stringify({ username: user || '', password: pw })
            );
        } catch(err) {}
    }

    // ── Stash / recall username across page navigations ──────────
    // Google (and similar) show email on page 1, password on page 2.
    function stashUsername(user) {
        if (user) {
            try { sessionStorage.setItem(STORAGE_KEY, user); } catch(e) {}
        }
    }

    function recallUsername() {
        try { return sessionStorage.getItem(STORAGE_KEY) || ''; } catch(e) { return ''; }
    }

    // ── Attempt to capture credentials ──────────────────────────
    function attemptCapture(scope) {
        var pw = findPassword(scope);
        if (!pw) return false;

        var user = findUsername(scope);
        if (!user) user = recallUsername();
        sendCreds(user, pw);
        return true;
    }

    // ── 1. Native form submit ────────────────────────────────────
    document.addEventListener('submit', function(e) {
        var form = e.target;
        if (!form || form.tagName !== 'FORM') return;
        // Before submitting: stash any username we see.
        var user = findUsername(form);
        stashUsername(user);
        attemptCapture(form);
    }, true);

    // ── 2. Click on any submit-like element (broad matching) ────
    //   Matches: <button>, <input type=submit>, [role="button"],
    //   and Google's jsaction-powered divs.
    document.addEventListener('click', function(e) {
        var el = e.target.closest(
            'button[type="submit"], input[type="submit"], ' +
            'button:not([type]), [role="button"], [jsaction]'
        );
        if (!el) return;

        // Check if there's a password field anywhere on the page.
        var pw = findPassword();
        if (pw) {
            // Password page — capture and send.
            var user = findUsername() || recallUsername();
            sendCreds(user, pw);
            return;
        }

        // No password field — maybe this is the "Next" button on the
        // username step (Google-style).  Stash the username.
        var user = findUsername();
        stashUsername(user);
    }, true);

    // ── 3. Enter key on password field ──────────────────────────
    document.addEventListener('keydown', function(e) {
        if (e.key !== 'Enter') return;
        var el = e.target;
        if (!el || el.tagName !== 'INPUT') return;
        if (el.type === 'password') {
            var user = findUsername() || recallUsername();
            sendCreds(user, el.value);
        } else if (el.type === 'text' || el.type === 'email' || el.type === 'tel') {
            // Pressing Enter on the username field — stash it.
            stashUsername(el.value);
        }
    }, true);

    // ── 4. Watch for dynamically inserted password fields ────────
    //   Some sites (Google) add the password input only after the
    //   username step completes.  When we see a new password field,
    //   we attach a listener so we catch Enter presses even on
    //   late-added inputs.
    var observer = new MutationObserver(function(mutations) {
        for (var i = 0; i < mutations.length; i++) {
            var added = mutations[i].addedNodes;
            for (var j = 0; j < added.length; j++) {
                var node = added[j];
                if (node.nodeType !== 1) continue;
                var pws = node.querySelectorAll
                    ? node.querySelectorAll('input[type="password"]')
                    : [];
                if (node.tagName === 'INPUT' && node.type === 'password') {
                    // Direct password input added.
                }
                // No action needed — the keydown/click listeners on
                // document already cover dynamically added elements.
            }
        }
    });
    observer.observe(document.documentElement, { childList: true, subtree: true });
})();"#;

/// Register the password capture message handler on the shared
/// UserContentManager.  Call this once during window setup.
///
/// When the injected JS detects a form submission with credentials,
/// it sends a JSON message to this handler, which then triggers the
/// "Save password?" prompt.
///
/// The capture JS is injected as a UserScript via the UCM so it runs
/// automatically on every page load, bypasses CSP, and works even on
/// pages like Google that have strict content security policies.
pub fn register_password_capture_handler(
    ucm: &UserContentManager,
    window: &adw::ApplicationWindow,
) {
    // Register the script message handler name.
    let _registered = ucm.register_script_message_handler("wynnPasswordCapture", None);

    // Inject the password capture JS as a UserScript so it runs on
    // every page load automatically (like the DNT / privacy scripts).
    // UserScripts bypass CSP and run in every frame.
    // Exclude Cloudflare challenge frames to avoid triggering bot detection.
    let capture_script = webkit6::UserScript::new(
        PASSWORD_CAPTURE_JS,
        webkit6::UserContentInjectedFrames::AllFrames,
        webkit6::UserScriptInjectionTime::End,
        &[],                    // allow-list: empty = all pages
        webview::CF_BLOCK_LIST, // block-list: skip Cloudflare challenge frames
    );
    ucm.add_script(&capture_script);

    let win = window.clone();

    // Rust-side deduplication: track recently prompted credentials to
    // suppress duplicate messages that slip through the JS guard
    // (e.g. across frame boundaries or page navigations like Google's
    // multi-step login that revisits the same origin).
    use std::cell::RefCell;
    use std::time::Instant;
    thread_local! {
        static LAST_PW_PROMPT: RefCell<(String, Instant)> = RefCell::new((String::new(), Instant::now()));
    }

    ucm.connect_script_message_received(Some("wynnPasswordCapture"), move |_ucm, js_value| {
        // The JS sends a JSON string: {"username": "...", "password": "..."}
        let json_str = js_value.to_str().to_string();

        // Parse the JSON manually (no serde dependency).
        let username = extract_json_field(&json_str, "username");
        let password = extract_json_field(&json_str, "password");

        if password.is_empty() {
            return;
        }

        // Determine origin from the currently active tab's URI.
        let origin = find_active_origin(&win);
        if origin.is_empty() {
            return;
        }

        if passwords::is_never_save(&origin) {
            return;
        }

        // Deduplicate: suppress identical prompts within 60 seconds.
        // This covers multi-step logins (Google), page reloads, and
        // rapid re-login attempts with the same credentials.
        let dedup_key = format!("{}\x00{}\x00{}", origin, username, password);
        let is_duplicate = LAST_PW_PROMPT.with(|cell| {
            let (ref last_key, ref last_time) = *cell.borrow();
            *last_key == dedup_key && last_time.elapsed().as_secs() < 60
        });
        if is_duplicate {
            return;
        }
        LAST_PW_PROMPT.with(|cell| {
            *cell.borrow_mut() = (dedup_key, Instant::now());
        });

        passwords::show_save_password_prompt(&win, &origin, &username, &password);
    });
}

/// Extract a string field value from a simple JSON object.
/// Handles escaped quotes within values.
fn extract_json_field(json: &str, field: &str) -> String {
    let pattern = format!("\"{}\"", field);
    if let Some(key_start) = json.find(&pattern) {
        let after_key = &json[key_start + pattern.len()..];
        // Skip whitespace and colon.
        let after_colon = match after_key.find(':') {
            Some(i) => &after_key[i + 1..],
            None => return String::new(),
        };
        let trimmed = after_colon.trim_start();
        if !trimmed.starts_with('"') {
            return String::new();
        }
        let value_start = &trimmed[1..];
        let mut result = String::new();
        let mut chars = value_start.chars();
        while let Some(c) = chars.next() {
            match c {
                '\\' => {
                    if let Some(escaped) = chars.next() {
                        result.push(escaped);
                    }
                }
                '"' => break,
                _ => result.push(c),
            }
        }
        result
    } else {
        String::new()
    }
}

/// Find the URI origin of the currently active tab in the window.
fn find_active_origin(window: &adw::ApplicationWindow) -> String {
    // Walk the widget tree to find the TabView, then get the selected page's WebView.
    fn find_tab_view(widget: &gtk4::Widget) -> Option<adw::TabView> {
        if let Some(tv) = widget.downcast_ref::<adw::TabView>() {
            return Some(tv.clone());
        }
        let mut child = widget.first_child();
        while let Some(c) = child {
            if let Some(found) = find_tab_view(&c) {
                return Some(found);
            }
            child = c.next_sibling();
        }
        None
    }

    if let Some(content) = window.content() {
        if let Some(tab_view) = find_tab_view(content.upcast_ref()) {
            if let Some(page) = tab_view.selected_page() {
                if let Ok(wv) = page.child().downcast::<WebView>() {
                    if let Some(uri) = wv.uri() {
                        return passwords::extract_origin(&uri);
                    }
                }
            }
        }
    }
    String::new()
}

// ── Public API ──────────────────────────────────────────────────────

/// Create a new browser tab inside `tab_view`.
///
/// A fresh WebView is created via [`webview::create_webview`], added
/// as a new page in the TabView, and immediately starts loading the
/// default homepage.
///
/// Per-tab signals are connected for:
/// - Tab title sync
/// - Address bar sync (selected tab only)
/// - Security icon (HTTPS lock indicator)
/// - Loading spinner + refresh/stop toggle
/// - Favicon display in the tab
/// - Progress bar in the header
/// - History recording on page load finish
///
/// Returns the `adw::TabPage` so the caller can create a sidebar row.
pub fn add_tab(
    tab_view: &adw::TabView,
    url_entry: &gtk4::Entry,
    progress_bar: &gtk4::ProgressBar,
    security_icon: &gtk4::Image,
    ucm: &UserContentManager,
) -> adw::TabPage {
    // ── 1. Create and configure the WebView ─────────────────────────
    let wv = webview::create_webview(ucm);

    // ── 2. Append to the TabView ────────────────────────────────────
    let page = tab_view.append(&wv);
    page.set_title("New Tab");

    // Set a default globe icon for the tab before favicon loads.
    let default_icon = gio::ThemedIcon::new("globe-symbolic");
    page.set_icon(Some(&default_icon));

    // ── 3. Load the homepage ────────────────────────────────────────
    let home = webview::home_url();
    webview::navigate_to(&wv, &home);

    // ── 4. Signal: page title changed ───────────────────────────────
    wv.connect_title_notify(clone!(
        #[weak]
        page,
        move |webview| {
            let title = webview
                .title()
                .filter(|t| !t.is_empty())
                .unwrap_or_else(|| "Untitled".into());
            page.set_title(&title);
        }
    ));

    // ── 5. Signal: URI changed ──────────────────────────────────────
    //   Re-inject autofill JS on SPA navigations (e.g. Google's
    //   multi-step login where the URL changes via pushState but no
    //   full page load occurs).
    wv.connect_uri_notify(clone!(
        #[weak]
        page,
        #[weak]
        url_entry,
        #[weak]
        security_icon,
        move |webview| {
            if let Some(uri) = webview.uri() {
                if page.is_selected() {
                    url_entry.set_text(&uri);
                    update_security_icon(&security_icon, &uri);
                }

                // Re-inject autofill for SPA navigations.
                // Password capture JS is handled via UCM UserScript.
                if uri.starts_with("http://") || uri.starts_with("https://") {
                    let origin = passwords::extract_origin(&uri);
                    if let Some(js) = passwords::autofill_js(&origin) {
                        webview.evaluate_javascript(
                            &js,
                            None,
                            None,
                            None::<&gio::Cancellable>,
                            |_| {},
                        );
                    }
                    // Don't prompt for unlock on SPA URI changes —
                    // that's handled by the LoadEvent::Finished path.
                }
            }
        }
    ));

    // ── 6. Signal: load state changed ───────────────────────────────
    wv.connect_load_changed(clone!(
        #[weak]
        page,
        #[weak]
        progress_bar,
        move |webview, event| {
            match event {
                LoadEvent::Started => {
                    page.set_loading(true);
                    if page.is_selected() {
                        progress_bar.set_fraction(0.0);
                        progress_bar.set_visible(true);
                    }
                }
                LoadEvent::Finished => {
                    page.set_loading(false);
                    if page.is_selected() {
                        progress_bar.set_visible(false);
                        progress_bar.set_fraction(0.0);

                        // Update the window title with the final page title.
                        if let Some(root) = webview.root() {
                            if let Some(window) = root.downcast_ref::<gtk4::Window>() {
                                let title = webview
                                    .title()
                                    .filter(|t| !t.is_empty())
                                    .map(|t| format!("{t} \u{2014} Wynn Browser"))
                                    .unwrap_or_else(|| "Wynn Browser".to_string());
                                window.set_title(Some(&title));
                            }
                        }
                    }

                    // Record the page visit in browsing history.
                    if let Some(uri) = webview.uri() {
                        let title = webview.title().map(|t| t.to_string()).unwrap_or_default();
                        history::record_visit(&uri, &title);

                        // Autofill JS injection (HTTP/HTTPS only).
                        // Password capture JS is handled via UCM UserScript.
                        if uri.starts_with("http://") || uri.starts_with("https://") {
                            let origin = passwords::extract_origin(&uri);
                            if let Some(js) = passwords::autofill_js(&origin) {
                                // Store is unlocked and we have creds — inject.
                                webview.evaluate_javascript(
                                    &js,
                                    None,
                                    None,
                                    None::<&gio::Cancellable>,
                                    |_| {},
                                );
                            } else if passwords::has_credentials_for(&origin) {
                                // Store is locked but creds exist — prompt
                                // for unlock, then inject autofill.
                                let wv = webview.clone();
                                let o = origin.clone();
                                let wv2 = webview.clone();
                                passwords::ensure_unlocked(&wv2, move || {
                                    if let Some(js) = passwords::autofill_js(&o) {
                                        wv.evaluate_javascript(
                                            &js,
                                            None,
                                            None,
                                            None::<&gio::Cancellable>,
                                            |_| {},
                                        );
                                    }
                                });
                            }
                        }
                    }
                }
                _ => {} // Redirected / Committed – keep spinning.
            }
        }
    ));

    // ── 7. Signal: estimated load progress changed ──────────────────
    wv.connect_estimated_load_progress_notify(clone!(
        #[weak]
        page,
        #[weak]
        progress_bar,
        move |webview| {
            if page.is_selected() {
                let progress = webview.estimated_load_progress();
                if webview.is_loading() {
                    progress_bar.set_fraction(progress);
                    progress_bar.set_visible(true);
                } else {
                    progress_bar.set_visible(false);
                    progress_bar.set_fraction(0.0);
                }
            }
        }
    ));

    // ── 8. Signal: favicon changed ──────────────────────────────────
    wv.connect_favicon_notify(clone!(
        #[weak]
        page,
        move |webview| {
            if let Some(texture) = webview.favicon() {
                let png_bytes = texture.save_to_png_bytes();
                let icon = gio::BytesIcon::new(&png_bytes);
                page.set_icon(Some(&icon));
            }
        }
    ));

    // ── 9. Signal: permission request → show allow/deny dialog ────
    wv.connect_permission_request(|webview, request| {
        // Determine human-readable name, icon, and description for this
        // permission type by downcasting the PermissionRequest interface.
        let (perm_name, icon_name, description) =
            if let Some(media) = request.downcast_ref::<webkit6::UserMediaPermissionRequest>() {
                let audio = media.is_for_audio_device();
                let video = media.is_for_video_device();
                match (audio, video) {
                    (true, true) => (
                        "Camera & Microphone",
                        "camera-video-symbolic",
                        "This site wants to use your camera and microphone.",
                    ),
                    (_, true) => (
                        "Camera",
                        "camera-video-symbolic",
                        "This site wants to use your camera.",
                    ),
                    (true, _) => (
                        "Microphone",
                        "audio-input-microphone-symbolic",
                        "This site wants to use your microphone.",
                    ),
                    _ => (
                        "Media Device",
                        "camera-video-symbolic",
                        "This site is requesting access to a media device.",
                    ),
                }
            } else if request
                .downcast_ref::<webkit6::GeolocationPermissionRequest>()
                .is_some()
            {
                (
                    "Location",
                    "find-location-symbolic",
                    "This site wants to know your location.",
                )
            } else if request
                .downcast_ref::<webkit6::NotificationPermissionRequest>()
                .is_some()
            {
                (
                    "Notifications",
                    "preferences-system-notifications-symbolic",
                    "This site wants to show you notifications.",
                )
            } else if request
                .downcast_ref::<webkit6::ClipboardPermissionRequest>()
                .is_some()
            {
                (
                    "Clipboard",
                    "edit-paste-symbolic",
                    "This site wants to access your clipboard.",
                )
            } else if request
                .downcast_ref::<webkit6::DeviceInfoPermissionRequest>()
                .is_some()
            {
                // Device enumeration (enumerateDevices) — auto-allow so sites
                // can detect available cameras/microphones.  This only reveals
                // device labels, not actual media streams.
                request.allow();
                return true;
            } else if request
                .downcast_ref::<webkit6::PointerLockPermissionRequest>()
                .is_some()
            {
                (
                    "Pointer Lock",
                    "input-mouse-symbolic",
                    "This site wants to lock your mouse pointer.",
                )
            } else if let Some(wda) =
                request.downcast_ref::<webkit6::WebsiteDataAccessPermissionRequest>()
            {
                // Third-party data access — allow for Cloudflare challenge
                // domains (needed for Turnstile verification), deny others.
                let requesting = wda
                    .requesting_domain()
                    .map(|d| d.to_string())
                    .unwrap_or_default();
                if requesting.ends_with("cloudflare.com")
                    || requesting.ends_with("cloudflareinsights.com")
                {
                    request.allow();
                } else {
                    request.deny();
                }
                return true;
            } else if request
                .downcast_ref::<webkit6::MediaKeySystemPermissionRequest>()
                .is_some()
            {
                // DRM key system (EME) — auto-allow for media playback.
                request.allow();
                return true;
            } else {
                (
                    "Permission",
                    "dialog-question-symbolic",
                    "This site is requesting a permission.",
                )
            };

        // Get the site origin for display.
        let origin = webview
            .uri()
            .map(|u| {
                // Extract scheme + host from the URI.
                let s = u.to_string();
                if let Some(idx) = s.find("://") {
                    let after_scheme = &s[idx + 3..];
                    if let Some(end) = after_scheme.find('/') {
                        s[..idx + 3 + end].to_string()
                    } else {
                        s.to_string()
                    }
                } else {
                    s.to_string()
                }
            })
            .unwrap_or_else(|| "Unknown site".to_string());

        // Build an adw::AlertDialog for the permission prompt.
        let dialog = adw::AlertDialog::builder()
            .heading(&format!("{perm_name} Access"))
            .body(&format!("{description}\n\nOrigin: {origin}"))
            .close_response("deny")
            .default_response("deny")
            .build();

        dialog.add_response("deny", "Deny");
        dialog.add_response("allow", "Allow");
        dialog.set_response_appearance("deny", adw::ResponseAppearance::Default);
        dialog.set_response_appearance("allow", adw::ResponseAppearance::Suggested);

        // Set an extra child with an icon for visual polish.
        let icon = gtk4::Image::from_icon_name(icon_name);
        icon.set_pixel_size(48);
        icon.set_margin_bottom(8);
        dialog.set_extra_child(Some(&icon));

        // Clone the request so we can act on it from the async response.
        let req = request.clone();

        // Connect the response signal to handle allow/deny.
        dialog.connect_response(None, move |_dialog, response| {
            if response == "allow" {
                req.allow();
            } else {
                req.deny();
            }
        });

        // Present the dialog in the window.
        if let Some(root) = webview.root() {
            if let Some(win) = root.downcast_ref::<gtk4::Window>() {
                dialog.present(Some(win));
            } else {
                // No window available — deny by default.
                request.deny();
            }
        } else {
            request.deny();
        }

        true // Signal handled.
    });

    // ── 10. Signal: permission state query (navigator.permissions.query) ──
    wv.connect_query_permission_state(|_webview, query| {
        // Return "prompt" for all permission types so sites know to ask.
        query.finish(PermissionState::Prompt);
        true
    });

    // ── 11. Signal: web notification → show via libnotify / toast ───
    wv.connect_show_notification(|webview, notification| {
        let title = notification
            .title()
            .unwrap_or_else(|| "Notification".into());
        let body = notification.body().unwrap_or_default();

        // Try to show via the GApplication notification system.
        if let Some(root) = webview.root() {
            if let Some(win) = root.downcast_ref::<adw::ApplicationWindow>() {
                if let Some(app) = win.application() {
                    let g_notif = gio::Notification::new(&title);
                    g_notif.set_body(Some(&body));
                    g_notif.set_icon(&gio::ThemedIcon::new(
                        "preferences-system-notifications-symbolic",
                    ));

                    // Use the notification ID (tag) if available, else a
                    // generated one based on the WebKit notification id.
                    let notif_id = notification
                        .tag()
                        .map(|t| t.to_string())
                        .unwrap_or_else(|| format!("web-notif-{}", notification.id()));

                    app.send_notification(Some(&notif_id), &g_notif);
                    return true;
                }
            }
        }

        // Fallback: show an in-browser toast.
        if let Some(root) = webview.root() {
            if let Some(win) = root.downcast_ref::<adw::ApplicationWindow>() {
                // Walk the widget tree to find the ToastOverlay.
                fn find_toast_overlay(widget: &gtk4::Widget) -> Option<adw::ToastOverlay> {
                    if let Some(overlay) = widget.downcast_ref::<adw::ToastOverlay>() {
                        return Some(overlay.clone());
                    }
                    let mut child = widget.first_child();
                    while let Some(c) = child {
                        if let Some(found) = find_toast_overlay(&c) {
                            return Some(found);
                        }
                        child = c.next_sibling();
                    }
                    None
                }

                if let Some(toast_overlay) = find_toast_overlay(win.content().unwrap().upcast_ref())
                {
                    let msg = if body.is_empty() {
                        title.to_string()
                    } else {
                        format!("{title}: {body}")
                    };
                    toast_overlay.add_toast(adw::Toast::new(&msg));
                    return true;
                }
            }
        }

        false // Let WebKit handle it (it won't, but signal the failure).
    });

    // ── 12. Signal: script dialog → native adw::AlertDialog ────────
    wv.connect_script_dialog(|webview, script_dialog| {
        let dialog_type = script_dialog.dialog_type();
        let message = script_dialog
            .message()
            .unwrap_or_else(|| "".into())
            .to_string();

        // Get site origin for the heading.
        let origin = webview
            .uri()
            .map(|u| {
                let s = u.to_string();
                if let Some(idx) = s.find("://") {
                    let after = &s[idx + 3..];
                    if let Some(end) = after.find('/') {
                        s[..idx + 3 + end].to_string()
                    } else {
                        s.to_string()
                    }
                } else {
                    s.to_string()
                }
            })
            .unwrap_or_else(|| "This page".to_string());

        // Clone the ScriptDialog so we can act on it from the response
        // callback.  ScriptDialog is ref-counted (Shared), so cloning
        // is cheap and keeps it alive until we call close().
        let sd = script_dialog.clone();

        match dialog_type {
            ScriptDialogType::Alert => {
                let dlg = adw::AlertDialog::builder()
                    .heading(&origin)
                    .body(&message)
                    .close_response("ok")
                    .default_response("ok")
                    .build();
                dlg.add_response("ok", "OK");
                dlg.set_response_appearance("ok", adw::ResponseAppearance::Suggested);

                let sd_cb = sd.clone();
                dlg.connect_response(None, move |_dlg, _response| {
                    sd_cb.close();
                });

                if let Some(root) = webview.root() {
                    if let Some(win) = root.downcast_ref::<gtk4::Window>() {
                        dlg.present(Some(win));
                    } else {
                        sd.close();
                    }
                } else {
                    sd.close();
                }
            }

            ScriptDialogType::Confirm => {
                let dlg = adw::AlertDialog::builder()
                    .heading(&origin)
                    .body(&message)
                    .close_response("cancel")
                    .default_response("ok")
                    .build();
                dlg.add_response("cancel", "Cancel");
                dlg.add_response("ok", "OK");
                dlg.set_response_appearance("ok", adw::ResponseAppearance::Suggested);

                let sd_cb = sd.clone();
                dlg.connect_response(None, move |_dlg, response| {
                    sd_cb.confirm_set_confirmed(response == "ok");
                    sd_cb.close();
                });

                if let Some(root) = webview.root() {
                    if let Some(win) = root.downcast_ref::<gtk4::Window>() {
                        dlg.present(Some(win));
                    } else {
                        sd.confirm_set_confirmed(false);
                        sd.close();
                    }
                } else {
                    sd.confirm_set_confirmed(false);
                    sd.close();
                }
            }

            ScriptDialogType::Prompt => {
                let default_text = script_dialog
                    .prompt_get_default_text()
                    .unwrap_or_else(|| "".into())
                    .to_string();

                let dlg = adw::AlertDialog::builder()
                    .heading(&origin)
                    .body(&message)
                    .close_response("cancel")
                    .default_response("ok")
                    .build();
                dlg.add_response("cancel", "Cancel");
                dlg.add_response("ok", "OK");
                dlg.set_response_appearance("ok", adw::ResponseAppearance::Suggested);

                // Add a text entry as the extra child.
                let entry = gtk4::Entry::new();
                entry.set_text(&default_text);
                entry.set_activates_default(true);
                entry.add_css_class("script-dialog-entry");
                dlg.set_extra_child(Some(&entry));

                let sd_cb = sd.clone();
                dlg.connect_response(None, move |_dlg, response| {
                    if response == "ok" {
                        sd_cb.prompt_set_text(&entry.text());
                    }
                    sd_cb.confirm_set_confirmed(response == "ok");
                    sd_cb.close();
                });

                if let Some(root) = webview.root() {
                    if let Some(win) = root.downcast_ref::<gtk4::Window>() {
                        dlg.present(Some(win));
                    } else {
                        sd.close();
                    }
                } else {
                    sd.close();
                }
            }

            ScriptDialogType::BeforeUnloadConfirm => {
                let dlg = adw::AlertDialog::builder()
                    .heading("Leave this page?")
                    .body(&message)
                    .close_response("stay")
                    .default_response("stay")
                    .build();
                dlg.add_response("stay", "Stay");
                dlg.add_response("leave", "Leave");
                dlg.set_response_appearance("leave", adw::ResponseAppearance::Destructive);

                let sd_cb = sd.clone();
                dlg.connect_response(None, move |_dlg, response| {
                    sd_cb.confirm_set_confirmed(response == "leave");
                    sd_cb.close();
                });

                if let Some(root) = webview.root() {
                    if let Some(win) = root.downcast_ref::<gtk4::Window>() {
                        dlg.present(Some(win));
                    } else {
                        sd.confirm_set_confirmed(false);
                        sd.close();
                    }
                } else {
                    sd.confirm_set_confirmed(false);
                    sd.close();
                }
            }

            _ => {
                // Unknown dialog type — let WebKit handle it.
                return false;
            }
        }

        true // Signal handled — suppress WebKit's default dialog.
    });

    // ── 13. Password capture via JS message handler ───────────────────
    //
    // Instead of using connect_submit_form (which calls list_text_fields
    // and segfaults on some sites like Google), we inject JavaScript that
    // intercepts form submissions with password fields and sends the
    // credentials back to Rust via the UserContentManager message handler
    // system.  The JS is injected on every page load in the LoadEvent::Finished
    // handler above; the message handler is registered once on the UCM in
    // window.rs via register_password_capture_handler().

    // ── 14. Signal: HTTP authentication challenge → login dialog ─────
    wv.connect_authenticate(|webview, auth_request| {
        // Skip proxy auth — let system handle it.
        if auth_request.is_for_proxy() {
            return false;
        }

        let host = auth_request
            .host()
            .map(|h| h.to_string())
            .unwrap_or_else(|| "Unknown host".to_string());
        let realm = auth_request
            .realm()
            .map(|r| r.to_string())
            .unwrap_or_default();
        let is_retry = auth_request.is_retry();

        // Construct an origin for credential lookup.
        let origin = webview
            .uri()
            .map(|u| passwords::extract_origin(&u))
            .unwrap_or_else(|| format!("https://{host}"));

        // Look up saved credentials for this origin.
        let saved = passwords::lookup_credentials(&origin);

        let heading = if is_retry {
            format!("Authentication Failed \u{2014} {host}")
        } else {
            format!("Sign in to {host}")
        };
        let body = if realm.is_empty() {
            "This server requires authentication.".to_string()
        } else {
            format!("Realm: {realm}")
        };

        let dialog = adw::AlertDialog::builder()
            .heading(&heading)
            .body(&body)
            .close_response("cancel")
            .default_response("sign-in")
            .build();

        dialog.add_response("cancel", "Cancel");
        dialog.add_response("sign-in", "Sign In");
        dialog.set_response_appearance("sign-in", adw::ResponseAppearance::Suggested);

        // Username and password entries.
        let form_box = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
        form_box.set_margin_top(4);

        let user_entry = gtk4::Entry::new();
        user_entry.set_placeholder_text(Some("Username"));
        user_entry.set_activates_default(true);
        form_box.append(&user_entry);

        let pw_entry = gtk4::PasswordEntry::new();
        pw_entry.set_placeholder_text(Some("Password"));
        pw_entry.set_show_peek_icon(true);
        form_box.append(&pw_entry);

        // Pre-fill with saved or proposed credentials.
        if let Some(cred) = saved.first() {
            user_entry.set_text(&cred.username);
            pw_entry.set_text(&cred.password);
            passwords::record_use(cred.id);
        } else if let Some(mut proposed) = auth_request.proposed_credential() {
            if let Some(u) = proposed.username() {
                user_entry.set_text(&u);
            }
            if let Some(p) = proposed.password() {
                pw_entry.set_text(&p);
            }
        }

        dialog.set_extra_child(Some(&form_box));

        let req = auth_request.clone();
        let origin_owned = origin.clone();

        dialog.connect_response(None, move |_dlg, response| {
            if response == "sign-in" {
                let u = user_entry.text().to_string();
                let p = pw_entry.text().to_string();
                let credential =
                    webkit6::Credential::new(&u, &p, webkit6::CredentialPersistence::None);
                req.authenticate(Some(&credential));

                // Save the credential for future use (if not a retry with
                // the same credentials, which would indicate wrong password).
                if !u.is_empty() && !p.is_empty() {
                    if !passwords::is_never_save(&origin_owned) {
                        passwords::save_credential(&origin_owned, &u, &p);
                    }
                }
            } else {
                req.cancel();
            }
        });

        if let Some(root) = webview.root() {
            if let Some(win) = root.downcast_ref::<gtk4::Window>() {
                dialog.present(Some(win));
            } else {
                auth_request.cancel();
            }
        } else {
            auth_request.cancel();
        }

        true // Signal handled.
    });

    // ── 15. Make this the active tab ────────────────────────────────
    tab_view.set_selected_page(&page);

    page
}

/// Return the [`WebView`] that belongs to the currently selected tab.
pub fn active_webview(tab_view: &adw::TabView) -> Option<WebView> {
    tab_view
        .selected_page()
        .map(|p| p.child())
        .and_then(|widget| widget.downcast::<WebView>().ok())
}

/// Update the security indicator icon based on the URI scheme.
pub fn update_security_icon(icon: &gtk4::Image, uri: &str) {
    icon.remove_css_class("security-secure");
    icon.remove_css_class("security-insecure");

    if uri.starts_with("https://") {
        icon.set_icon_name(Some("channel-secure-symbolic"));
        icon.add_css_class("security-secure");
    } else if uri.starts_with("http://") {
        icon.set_icon_name(Some("channel-insecure-symbolic"));
        icon.add_css_class("security-insecure");
    } else {
        icon.set_icon_name(Some("system-search-symbolic"));
    }
}
