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
    wv.connect_uri_notify(clone!(
        #[weak]
        page,
        #[weak]
        url_entry,
        #[weak]
        security_icon,
        move |webview| {
            if page.is_selected() {
                if let Some(uri) = webview.uri() {
                    url_entry.set_text(&uri);
                    update_security_icon(&security_icon, &uri);
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

                        // Inject autofill JS for saved passwords (HTTP/HTTPS only).
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
                // Third-party data access — auto-deny for privacy.
                let _requesting = wda.requesting_domain();
                let _current = wda.current_domain();
                request.deny();
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

    // ── 13. Signal: form submission → detect & save passwords ────────
    wv.connect_submit_form(|webview, form_request| {
        // Extract text fields from the form BEFORE calling submit().
        let fields = form_request.list_text_fields();

        // Always let the form submit proceed.
        form_request.submit();

        let (names, values) = match fields {
            Some(pair) if !pair.0.is_empty() => pair,
            _ => return,
        };

        // Find the password field(s) and username field.
        let mut password = String::new();
        let mut username = String::new();

        let pw_hints = ["pass", "pwd", "password", "passwd", "secret"];
        let user_hints = ["user", "email", "login", "name", "account", "id", "uname"];

        for (i, name) in names.iter().enumerate() {
            let n = name.to_lowercase();
            let val = values.get(i).map(|v| v.to_string()).unwrap_or_default();

            if pw_hints.iter().any(|h| n.contains(h)) {
                if !val.is_empty() {
                    password = val;
                }
            } else if user_hints.iter().any(|h| n.contains(h)) {
                if !val.is_empty() {
                    username = val;
                }
            }
        }

        // If no username found by name hints, use the first non-password
        // field that has a value (common for forms with generic field names).
        if username.is_empty() {
            for (i, name) in names.iter().enumerate() {
                let n = name.to_lowercase();
                if !pw_hints.iter().any(|h| n.contains(h)) {
                    let val = values.get(i).map(|v| v.to_string()).unwrap_or_default();
                    if !val.is_empty() {
                        username = val;
                        break;
                    }
                }
            }
        }

        if password.is_empty() {
            return; // No password field — not a login form.
        }

        // Get the page origin.
        let origin = webview
            .uri()
            .map(|u| passwords::extract_origin(&u))
            .unwrap_or_default();

        if origin.is_empty() {
            return;
        }

        // Check if user opted out of saving for this origin.
        if passwords::is_never_save(&origin) {
            return;
        }

        // Show the save/update password prompt.
        if let Some(root) = webview.root() {
            if let Some(win) = root.downcast_ref::<gtk4::Window>() {
                passwords::show_save_password_prompt(win, &origin, &username, &password);
            }
        }
    });

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
