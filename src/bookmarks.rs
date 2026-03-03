// ─────────────────────────────────────────────────────────────────────
// bookmarks.rs  –  Bookmark management for Wynn Browser
// ─────────────────────────────────────────────────────────────────────
//
// Public API:
//
//   add_bookmark(url, title)        – save a URL as a bookmark.
//   remove_bookmark(id)             – delete by ID.
//   remove_bookmark_by_url(url)     – delete by URL.
//   is_bookmarked(url)              – check if a URL is bookmarked.
//   all_bookmarks()                 – list all bookmarks.
//   toggle_bookmark(url, title)     – add or remove.
//   show_bookmarks_dialog()         – present a dialog listing bookmarks.
// ─────────────────────────────────────────────────────────────────────

use crate::database;
use glib::clone;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

/// A single bookmark entry for display purposes.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct Bookmark {
    pub id: i64,
    pub url: String,
    pub title: String,
    pub folder: String,
    pub created: String,
}

/// Add a new bookmark.  Does nothing if the URL is already bookmarked.
pub fn add_bookmark(url: &str, title: &str) {
    if url.is_empty() || is_bookmarked(url) {
        return;
    }
    database::with_db(|conn| {
        let _ = conn.execute(
            "INSERT INTO bookmarks (url, title) VALUES (?1, ?2)",
            rusqlite::params![url, title],
        );
    });
}

/// Remove a bookmark by its database ID.
pub fn remove_bookmark(id: i64) {
    database::with_db(|conn| {
        let _ = conn.execute("DELETE FROM bookmarks WHERE id = ?1", rusqlite::params![id]);
    });
}

/// Remove a bookmark by URL.
pub fn remove_bookmark_by_url(url: &str) {
    database::with_db(|conn| {
        let _ = conn.execute(
            "DELETE FROM bookmarks WHERE url = ?1",
            rusqlite::params![url],
        );
    });
}

/// Check whether a URL is bookmarked.
pub fn is_bookmarked(url: &str) -> bool {
    database::with_db(|conn| {
        conn.query_row(
            "SELECT COUNT(*) FROM bookmarks WHERE url = ?1",
            rusqlite::params![url],
            |row| row.get::<_, i64>(0),
        )
        .unwrap_or(0)
            > 0
    })
}

/// Toggle: add the bookmark if not present, remove if present.
/// Returns `true` if the bookmark now exists (was added).
pub fn toggle_bookmark(url: &str, title: &str) -> bool {
    if is_bookmarked(url) {
        remove_bookmark_by_url(url);
        false
    } else {
        add_bookmark(url, title);
        true
    }
}

/// Return all bookmarks, newest first.
pub fn all_bookmarks() -> Vec<Bookmark> {
    database::with_db(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT id, url, title, folder, created
                   FROM bookmarks
                  ORDER BY created DESC",
            )
            .expect("Failed to prepare bookmarks query");

        stmt.query_map([], |row| {
            Ok(Bookmark {
                id: row.get(0)?,
                url: row.get(1)?,
                title: row.get(2)?,
                folder: row.get(3)?,
                created: row.get(4)?,
            })
        })
        .expect("Failed to query bookmarks")
        .filter_map(|r| r.ok())
        .collect()
    })
}

/// Present a dialog showing all bookmarks.
///
/// Clicking an entry navigates the active tab to that URL.
pub fn show_bookmarks_dialog(window: &adw::ApplicationWindow, tab_view: &adw::TabView) {
    let dialog = gtk4::Window::builder()
        .title("Bookmarks")
        .transient_for(window)
        .modal(true)
        .default_width(600)
        .default_height(500)
        .build();

    let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 0);

    // ── Header bar ──────────────────────────────────────────────────
    let header = adw::HeaderBar::new();
    vbox.append(&header);

    // ── Scrollable list ─────────────────────────────────────────────
    let list_box = gtk4::ListBox::new();
    list_box.set_selection_mode(gtk4::SelectionMode::None);
    list_box.add_css_class("boxed-list");
    list_box.set_margin_start(12);
    list_box.set_margin_end(12);
    list_box.set_margin_top(8);
    list_box.set_margin_bottom(12);

    let scrolled = gtk4::ScrolledWindow::new();
    scrolled.set_vexpand(true);
    scrolled.set_child(Some(&list_box));
    vbox.append(&scrolled);

    dialog.set_child(Some(&vbox));

    // ── Populate ────────────────────────────────────────────────────
    let bookmarks = all_bookmarks();

    if bookmarks.is_empty() {
        let label = gtk4::Label::new(Some("No bookmarks yet"));
        label.add_css_class("dim-label");
        label.set_margin_top(24);
        label.set_margin_bottom(24);
        list_box.append(&label);
    } else {
        for bm in &bookmarks {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&bm.title))
                .subtitle(glib::markup_escape_text(&bm.url))
                .activatable(true)
                .build();

            // Delete button suffix.
            let del_btn = gtk4::Button::from_icon_name("user-trash-symbolic");
            del_btn.set_valign(gtk4::Align::Center);
            del_btn.add_css_class("flat");
            del_btn.set_tooltip_text(Some("Remove bookmark"));

            let bm_id = bm.id;
            del_btn.connect_clicked(clone!(
                #[weak]
                row,
                #[weak]
                list_box,
                move |_| {
                    remove_bookmark(bm_id);
                    list_box.remove(&row);
                }
            ));
            row.add_suffix(&del_btn);

            // Navigate on click.
            let url = bm.url.clone();
            let tv = tab_view.clone();
            let dlg = dialog.clone();
            row.connect_activated(move |_| {
                if let Some(wv) = crate::browser_tab::active_webview(&tv) {
                    crate::webview::navigate_to(&wv, &url);
                }
                dlg.close();
            });

            list_box.append(&row);
        }
    }

    dialog.present();
}
