// ─────────────────────────────────────────────────────────────────────
// settings.rs  –  Preferences window for Wynn Browser (Phase 3)
// ─────────────────────────────────────────────────────────────────────
//
// Uses `adw::PreferencesWindow` with pages for:
//
//   General    – homepage URL, search engine selection.
//   Privacy    – ITP toggle, cookie policy, DNT, WebRTC, custom UA,
//                clear history, clear cookies.
//   Content    – ad blocker toggle, blocklist info, refresh button.
//   Appearance – sidebar auto-show.
//
// Settings are persisted in the SQLite `settings` table.
//
// Public API:
//
//   show_settings(window, ucm) – present the preferences window.
//   get_setting(key)           – read a setting from the DB.
//   set_setting(key, value)    – write a setting to the DB.
//   homepage()                 – current homepage URL.
//   search_engine_url()        – current search URL template.
// ─────────────────────────────────────────────────────────────────────

use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use crate::adblocker;
use crate::database;
use crate::history;
use crate::keepassxc;

// ── Settings helpers ────────────────────────────────────────────────

/// Read a setting from the database.  Returns `None` if not set.
pub fn get_setting(key: &str) -> Option<String> {
    database::with_db(|conn| {
        conn.query_row(
            "SELECT value FROM settings WHERE key = ?1",
            rusqlite::params![key],
            |row| row.get::<_, String>(0),
        )
        .ok()
    })
}

/// Write a setting to the database (upsert).
pub fn set_setting(key: &str, value: &str) {
    database::with_db(|conn| {
        let _ = conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES (?1, ?2)",
            rusqlite::params![key, value],
        );
    });
}

/// Return the configured homepage URL (default: DuckDuckGo).
pub fn homepage() -> String {
    get_setting("homepage").unwrap_or_else(|| "https://duckduckgo.com".to_string())
}

/// Return the configured search engine URL prefix.
pub fn search_engine_url() -> String {
    get_setting("search_engine").unwrap_or_else(|| "https://duckduckgo.com/?q=".to_string())
}

// ── Preferences window ─────────────────────────────────────────────

/// Present the preferences window.
pub fn show_settings(window: &adw::ApplicationWindow, ucm: &webkit6::UserContentManager) {
    #[allow(deprecated)]
    let prefs = adw::PreferencesWindow::builder()
        .title("Settings")
        .transient_for(window)
        .modal(true)
        .default_width(650)
        .default_height(550)
        .build();

    // ════════════════════════════════════════════════════════════════
    //  GENERAL PAGE
    // ════════════════════════════════════════════════════════════════
    let general_page = adw::PreferencesPage::builder()
        .title("General")
        .icon_name("preferences-system-symbolic")
        .build();

    // Homepage group.
    let homepage_group = adw::PreferencesGroup::builder()
        .title("Homepage")
        .description("The page loaded when you open a new tab")
        .build();

    let homepage_row = adw::EntryRow::builder()
        .title("Homepage URL")
        .text(homepage())
        .show_apply_button(true)
        .build();

    homepage_row.connect_apply(|row| {
        let text: String = row.text().into();
        if !text.is_empty() {
            set_setting("homepage", &text);
        }
    });

    homepage_group.add(&homepage_row);
    general_page.add(&homepage_group);

    // Search engine group.
    let search_group = adw::PreferencesGroup::builder()
        .title("Search Engine")
        .build();

    let search_model = gtk4::StringList::new(&["DuckDuckGo", "Google", "Bing", "Brave Search"]);

    let search_row = adw::ComboRow::builder()
        .title("Default search engine")
        .model(&search_model)
        .build();

    let current_search = search_engine_url();
    let selected = if current_search.contains("google.com") {
        1
    } else if current_search.contains("bing.com") {
        2
    } else if current_search.contains("brave.com") || current_search.contains("search.brave") {
        3
    } else {
        0
    };
    search_row.set_selected(selected);

    search_row.connect_selected_notify(|row| {
        let url = match row.selected() {
            1 => "https://www.google.com/search?q=",
            2 => "https://www.bing.com/search?q=",
            3 => "https://search.brave.com/search?q=",
            _ => "https://duckduckgo.com/?q=",
        };
        set_setting("search_engine", url);
    });

    search_group.add(&search_row);
    general_page.add(&search_group);

    #[allow(deprecated)]
    prefs.add(&general_page);

    // ════════════════════════════════════════════════════════════════
    //  PRIVACY PAGE
    // ════════════════════════════════════════════════════════════════
    let privacy_page = adw::PreferencesPage::builder()
        .title("Privacy")
        .icon_name("security-high-symbolic")
        .build();

    // ── Tracking Protection group ───────────────────────────────────
    let tracking_group = adw::PreferencesGroup::builder()
        .title("Tracking Protection")
        .description("Controls to limit tracking across websites")
        .build();

    // ITP toggle.
    let itp_enabled = get_setting("privacy_itp")
        .map(|v| v == "true")
        .unwrap_or(true);

    let itp_row = adw::SwitchRow::builder()
        .title("Intelligent Tracking Prevention")
        .subtitle("WebKit's built-in cross-site tracking blocker")
        .active(itp_enabled)
        .build();

    itp_row.connect_active_notify(|row| {
        let val = if row.is_active() { "true" } else { "false" };
        set_setting("privacy_itp", val);
    });
    tracking_group.add(&itp_row);

    // DNT toggle.
    let dnt_enabled = get_setting("privacy_dnt")
        .map(|v| v == "true")
        .unwrap_or(true);

    let dnt_row = adw::SwitchRow::builder()
        .title("Do Not Track")
        .subtitle("Send DNT and Global Privacy Control signals")
        .active(dnt_enabled)
        .build();

    dnt_row.connect_active_notify(|row| {
        let val = if row.is_active() { "true" } else { "false" };
        set_setting("privacy_dnt", val);
    });
    tracking_group.add(&dnt_row);

    // Custom User Agent toggle.
    let ua_enabled = get_setting("privacy_custom_ua")
        .map(|v| v == "true")
        .unwrap_or(true);

    let ua_row = adw::SwitchRow::builder()
        .title("Custom User Agent")
        .subtitle("Use a privacy-respecting Safari-compatible user agent string")
        .active(ua_enabled)
        .build();

    ua_row.connect_active_notify(|row| {
        let val = if row.is_active() { "true" } else { "false" };
        set_setting("privacy_custom_ua", val);
    });
    tracking_group.add(&ua_row);

    privacy_page.add(&tracking_group);

    // ── Cookie Policy group ─────────────────────────────────────────
    let cookie_group = adw::PreferencesGroup::builder().title("Cookies").build();

    let cookie_model =
        gtk4::StringList::new(&["Block Third-Party (Recommended)", "Allow All", "Block All"]);

    let cookie_row = adw::ComboRow::builder()
        .title("Cookie Policy")
        .subtitle("Control which cookies are accepted")
        .model(&cookie_model)
        .build();

    let current_cookie =
        get_setting("privacy_cookies").unwrap_or_else(|| "no-third-party".to_string());
    let cookie_selected = match current_cookie.as_str() {
        "allow-all" => 1,
        "block-all" => 2,
        _ => 0, // no-third-party
    };
    cookie_row.set_selected(cookie_selected);

    cookie_row.connect_selected_notify(|row| {
        let policy = match row.selected() {
            1 => "allow-all",
            2 => "block-all",
            _ => "no-third-party",
        };
        set_setting("privacy_cookies", policy);
    });

    cookie_group.add(&cookie_row);
    privacy_page.add(&cookie_group);

    // ── WebRTC group ────────────────────────────────────────────────
    let webrtc_group = adw::PreferencesGroup::builder().title("WebRTC").build();

    let webrtc_enabled = get_setting("privacy_webrtc")
        .map(|v| v == "true")
        .unwrap_or(false);

    let webrtc_row = adw::SwitchRow::builder()
        .title("Enable WebRTC")
        .subtitle("Required for video calls; may leak your IP address")
        .active(webrtc_enabled)
        .build();

    webrtc_row.connect_active_notify(|row| {
        let val = if row.is_active() { "true" } else { "false" };
        set_setting("privacy_webrtc", val);
    });
    webrtc_group.add(&webrtc_row);

    let webrtc_leak = get_setting("privacy_webrtc_leak")
        .map(|v| v == "true")
        .unwrap_or(true);

    let webrtc_leak_row = adw::SwitchRow::builder()
        .title("WebRTC IP Leak Prevention")
        .subtitle("Force relay mode to hide your real IP in WebRTC")
        .active(webrtc_leak)
        .build();

    webrtc_leak_row.connect_active_notify(|row| {
        let val = if row.is_active() { "true" } else { "false" };
        set_setting("privacy_webrtc_leak", val);
    });
    webrtc_group.add(&webrtc_leak_row);

    privacy_page.add(&webrtc_group);

    // ── Browsing Data group ─────────────────────────────────────────
    let data_group = adw::PreferencesGroup::builder()
        .title("Browsing Data")
        .description("Clear stored browsing data")
        .build();

    let clear_history_row = adw::ActionRow::builder()
        .title("Clear Browsing History")
        .subtitle("Remove all visited page records")
        .activatable(true)
        .build();

    let clear_icon = gtk4::Image::from_icon_name("user-trash-symbolic");
    clear_history_row.add_suffix(&clear_icon);

    clear_history_row.connect_activated(|row| {
        history::clear_all();
        row.set_subtitle("History cleared");
    });

    data_group.add(&clear_history_row);
    privacy_page.add(&data_group);

    // ── Password Backend group ──────────────────────────────────────
    let password_group = adw::PreferencesGroup::builder()
        .title("Password Manager")
        .description("Choose how passwords are stored and autofilled")
        .build();

    let pw_backend_model = gtk4::StringList::new(&["Built-in (Wynn)", "KeePassXC"]);

    let pw_backend_row = adw::ComboRow::builder()
        .title("Password Backend")
        .subtitle("Select which password manager to use")
        .model(&pw_backend_model)
        .build();

    let current_backend = keepassxc::backend_name();
    pw_backend_row.set_selected(if current_backend == "keepassxc" { 1 } else { 0 });

    // Status row — shows KeePassXC connection state.
    let kpxc_status_row = adw::ActionRow::builder()
        .title("KeePassXC Status")
        .subtitle(if keepassxc::is_active_backend() {
            if keepassxc::is_connected() {
                "Connected"
            } else if keepassxc::is_available() {
                "Available — will connect on first use"
            } else {
                "Not running — start KeePassXC and enable browser integration"
            }
        } else {
            "Not active — select KeePassXC above to enable"
        })
        .build();

    let status_icon_name = if keepassxc::is_active_backend() && keepassxc::is_connected() {
        "emblem-ok-symbolic"
    } else if keepassxc::is_active_backend() && keepassxc::is_available() {
        "emblem-synchronizing-symbolic"
    } else {
        "dialog-information-symbolic"
    };
    let status_icon = gtk4::Image::from_icon_name(status_icon_name);
    kpxc_status_row.add_suffix(&status_icon);

    // Connect button — only useful when KeePassXC backend is active.
    let connect_row = adw::ActionRow::builder()
        .title("Connect to KeePassXC")
        .subtitle("Establish connection and pair with database")
        .activatable(true)
        .build();

    let connect_icon = gtk4::Image::from_icon_name("network-wired-symbolic");
    connect_row.add_suffix(&connect_icon);

    let kpxc_status_row_ref = kpxc_status_row.clone();
    let status_icon_ref = status_icon.clone();
    connect_row.connect_activated(move |row| {
        if !keepassxc::is_active_backend() {
            row.set_subtitle("Select KeePassXC as the backend first");
            return;
        }
        match keepassxc::connect() {
            Ok(()) => {
                row.set_subtitle("Connected successfully!");
                kpxc_status_row_ref.set_subtitle("Connected");
                status_icon_ref.set_icon_name(Some("emblem-ok-symbolic"));
            }
            Err(e) => {
                row.set_subtitle(&format!("Failed: {}", e));
                kpxc_status_row_ref.set_subtitle("Connection failed");
                status_icon_ref.set_icon_name(Some("dialog-error-symbolic"));
            }
        }
    });

    let kpxc_status_row_ref2 = kpxc_status_row.clone();
    let status_icon_ref2 = status_icon.clone();
    pw_backend_row.connect_selected_notify(move |row| {
        let backend = match row.selected() {
            1 => "keepassxc",
            _ => "builtin",
        };
        keepassxc::set_backend(backend);
        // Update status display.
        if backend == "keepassxc" {
            if keepassxc::is_available() {
                kpxc_status_row_ref2.set_subtitle(
                    "Available — use Connect button or will auto-connect on first use",
                );
                status_icon_ref2.set_icon_name(Some("emblem-synchronizing-symbolic"));
            } else {
                kpxc_status_row_ref2
                    .set_subtitle("Not running — start KeePassXC and enable browser integration");
                status_icon_ref2.set_icon_name(Some("dialog-warning-symbolic"));
            }
        } else {
            kpxc_status_row_ref2.set_subtitle("Not active — select KeePassXC above to enable");
            status_icon_ref2.set_icon_name(Some("dialog-information-symbolic"));
        }
    });

    password_group.add(&pw_backend_row);
    password_group.add(&kpxc_status_row);
    password_group.add(&connect_row);
    privacy_page.add(&password_group);

    #[allow(deprecated)]
    prefs.add(&privacy_page);

    // ════════════════════════════════════════════════════════════════
    //  CONTENT PAGE (Ad Blocker)
    // ════════════════════════════════════════════════════════════════
    let content_page = adw::PreferencesPage::builder()
        .title("Content")
        .icon_name("applications-internet-symbolic")
        .build();

    let adblock_group = adw::PreferencesGroup::builder()
        .title("Ad Blocking")
        .description("Block ads and tracking scripts using Pete Lowe's blocklist")
        .build();

    let adblock_row = adw::SwitchRow::builder()
        .title("Enable Ad Blocker")
        .subtitle("Blocks ads, trackers, and annoyances")
        .active(adblocker::is_enabled())
        .build();

    let ucm_clone = ucm.clone();
    adblock_row.connect_active_notify(move |row| {
        let enabled = row.is_active();
        adblocker::set_enabled(enabled);
        if enabled {
            adblocker::load_rules(&ucm_clone);
        } else {
            adblocker::clear_rules(&ucm_clone);
        }
    });

    adblock_group.add(&adblock_row);

    // Blocklist info row.
    let domain_count = adblocker::blocklist_domain_count();
    let info_subtitle = if domain_count > 0 {
        format!("{} domains in blocklist", domain_count)
    } else {
        "No blocklist cached — use Refresh to download".to_string()
    };

    let blocklist_info_row = adw::ActionRow::builder()
        .title("Blocklist Status")
        .subtitle(info_subtitle)
        .build();

    let refresh_icon = gtk4::Image::from_icon_name("view-refresh-symbolic");
    blocklist_info_row.add_suffix(&refresh_icon);

    adblock_group.add(&blocklist_info_row);
    content_page.add(&adblock_group);

    #[allow(deprecated)]
    prefs.add(&content_page);

    // ════════════════════════════════════════════════════════════════
    //  APPEARANCE PAGE
    // ════════════════════════════════════════════════════════════════
    let appearance_page = adw::PreferencesPage::builder()
        .title("Appearance")
        .icon_name("preferences-desktop-appearance-symbolic")
        .build();

    let sidebar_group = adw::PreferencesGroup::builder().title("Sidebar").build();

    let sidebar_position_model = gtk4::StringList::new(&["Left", "Right"]);
    let sidebar_position_row = adw::ComboRow::builder()
        .title("Sidebar Position")
        .subtitle("Which side of the window the tab sidebar appears")
        .model(&sidebar_position_model)
        .build();

    let current_pos = get_setting("sidebar_position").unwrap_or_else(|| "left".to_string());
    sidebar_position_row.set_selected(if current_pos == "right" { 1 } else { 0 });

    sidebar_position_row.connect_selected_notify(|row| {
        let pos = match row.selected() {
            1 => "right",
            _ => "left",
        };
        set_setting("sidebar_position", pos);
    });

    sidebar_group.add(&sidebar_position_row);
    appearance_page.add(&sidebar_group);

    #[allow(deprecated)]
    prefs.add(&appearance_page);

    // ── Present ─────────────────────────────────────────────────────
    #[allow(deprecated)]
    prefs.present();
}
