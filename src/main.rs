// ─────────────────────────────────────────────────────────────────────
// main.rs  –  Application entry point for Wynn Browser
// ─────────────────────────────────────────────────────────────────────
//
// This file does three things:
//
//   1. Declares the module tree so rustc knows about our source files.
//   2. Creates an `adw::Application` – the libadwaita flavour of
//      `gtk4::Application`.  This handles GTK initialisation, the
//      main event loop, and D-Bus application uniqueness.
//   3. Connects the `activate` signal so we build the browser window
//      the first time the app is launched (or re-focused).
//
// Using `adw::Application` instead of `gtk4::Application` ensures
// that libadwaita's stylesheet, colour scheme, and adaptive widgets
// are all initialised automatically.
// ─────────────────────────────────────────────────────────────────────

// ── Module declarations ─────────────────────────────────────────────
// Each module lives in its own file under src/.
mod adblocker; // Basic ad blocking via UserContentManager.
mod bookmarks; // Bookmark management and UI dialog.
mod browser_tab; // Per-tab state: wraps a WebView + metadata.
mod command_palette; // VS Code-style command palette overlay.
mod database; // SQLite connection and schema management.
mod downloads; // Download manager and progress tracking.
mod history; // Browsing history recording and UI dialog.
mod passwords; // Password manager: save, autofill, management UI.
mod settings; // Preferences window and settings persistence.
mod webview; // WebView factory and URI normalisation helpers.
mod window; // Main window construction and signal wiring.

// ── Crate imports ───────────────────────────────────────────────────
use libadwaita as adw;
use libadwaita::prelude::*;

/// Reverse-domain application ID.
///
/// GNOME uses this to identify the app on D-Bus, locate resources
/// (icons, .desktop files, GSettings schemas), and enforce single-
/// instance behaviour.
const APP_ID: &str = "com.wynn.Browser";

fn main() {
    // Build the Application object.
    //
    // `adw::Application` is a thin wrapper around `gtk4::Application`
    // that additionally calls `adw::init()` for us.  The builder
    // pattern lets us set the application ID without touching flags
    // or other properties we do not need yet.
    let app = adw::Application::builder().application_id(APP_ID).build();

    // The `activate` signal fires:
    //   - On the very first launch.
    //   - When the user tries to open a second instance (the existing
    //     instance receives the signal instead).
    //
    // We use it to construct (or re-present) the browser window.
    app.connect_activate(|app| {
        window::build_window(app);
    });

    // `run()` enters the GTK main loop.  It blocks until every window
    // is closed or `app.quit()` is called.  Command-line arguments
    // are forwarded automatically from `std::env::args`.
    app.run();
}
