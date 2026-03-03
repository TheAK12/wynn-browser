// ─────────────────────────────────────────────────────────────────────
// history.rs  –  Browsing history system for Wynn Browser
// ─────────────────────────────────────────────────────────────────────
//
// Public API:
//
//   record_visit(url, title) – insert or update a history entry.
//   search(query)            – full-text search over URL + title.
//   all_entries(limit)       – most-recent entries.
//   clear_all()              – delete everything.
//   show_history_dialog()    – present a GTK dialog listing history.
// ─────────────────────────────────────────────────────────────────────

use crate::database;
use glib::clone;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

/// A single history entry for display purposes.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct HistoryEntry {
    pub id: i64,
    pub url: String,
    pub title: String,
    pub visit_count: i64,
    pub last_visit: String,
}

/// Record a page visit.  If the URL already exists, increment
/// `visit_count` and update `last_visit` and `title`.
pub fn record_visit(url: &str, title: &str) {
    // Skip internal / blank pages.
    if url.is_empty() || url == "about:blank" {
        return;
    }

    database::with_db(|conn| {
        // Try to update an existing row first.
        let updated = conn
            .execute(
                "UPDATE history
                    SET visit_count = visit_count + 1,
                        last_visit  = datetime('now'),
                        title       = ?2
                  WHERE url = ?1",
                rusqlite::params![url, title],
            )
            .unwrap_or(0);

        // If nothing was updated, insert a new row.
        if updated == 0 {
            let _ = conn.execute(
                "INSERT INTO history (url, title) VALUES (?1, ?2)",
                rusqlite::params![url, title],
            );
        }
    });
}

/// Search history by URL or title (case-insensitive substring match).
pub fn search(query: &str) -> Vec<HistoryEntry> {
    let pattern = format!("%{query}%");
    database::with_db(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT id, url, title, visit_count, last_visit
                   FROM history
                  WHERE url   LIKE ?1
                     OR title LIKE ?1
                  ORDER BY last_visit DESC
                  LIMIT 200",
            )
            .expect("Failed to prepare history search");

        stmt.query_map(rusqlite::params![pattern], |row| {
            Ok(HistoryEntry {
                id: row.get(0)?,
                url: row.get(1)?,
                title: row.get(2)?,
                visit_count: row.get(3)?,
                last_visit: row.get(4)?,
            })
        })
        .expect("Failed to query history")
        .filter_map(|r| r.ok())
        .collect()
    })
}

/// Return the most recent history entries.
pub fn all_entries(limit: u32) -> Vec<HistoryEntry> {
    database::with_db(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT id, url, title, visit_count, last_visit
                   FROM history
                  ORDER BY last_visit DESC
                  LIMIT ?1",
            )
            .expect("Failed to prepare history query");

        stmt.query_map(rusqlite::params![limit], |row| {
            Ok(HistoryEntry {
                id: row.get(0)?,
                url: row.get(1)?,
                title: row.get(2)?,
                visit_count: row.get(3)?,
                last_visit: row.get(4)?,
            })
        })
        .expect("Failed to query history")
        .filter_map(|r| r.ok())
        .collect()
    })
}

/// Delete all history entries.
pub fn clear_all() {
    database::with_db(|conn| {
        let _ = conn.execute("DELETE FROM history", []);
    });
}

/// Present a dialog showing browsing history.
///
/// Clicking an entry navigates the active tab to that URL.
pub fn show_history_dialog(window: &adw::ApplicationWindow, tab_view: &adw::TabView) {
    let dialog = gtk4::Window::builder()
        .title("History")
        .transient_for(window)
        .modal(true)
        .default_width(600)
        .default_height(500)
        .build();

    let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 0);

    // ── Header bar with clear button ────────────────────────────────
    let header = adw::HeaderBar::new();
    let clear_btn = gtk4::Button::with_label("Clear All");
    clear_btn.add_css_class("destructive-action");
    header.pack_end(&clear_btn);
    vbox.append(&header);

    // ── Search entry ────────────────────────────────────────────────
    let search_entry = gtk4::SearchEntry::new();
    search_entry.set_placeholder_text(Some("Search history\u{2026}"));
    search_entry.set_margin_start(12);
    search_entry.set_margin_end(12);
    search_entry.set_margin_top(8);
    search_entry.set_margin_bottom(8);
    vbox.append(&search_entry);

    // ── Scrollable list ─────────────────────────────────────────────
    let list_box = gtk4::ListBox::new();
    list_box.set_selection_mode(gtk4::SelectionMode::None);
    list_box.add_css_class("boxed-list");
    list_box.set_margin_start(12);
    list_box.set_margin_end(12);
    list_box.set_margin_bottom(12);

    let scrolled = gtk4::ScrolledWindow::new();
    scrolled.set_vexpand(true);
    scrolled.set_child(Some(&list_box));
    vbox.append(&scrolled);

    dialog.set_child(Some(&vbox));

    // ── Populate the list ───────────────────────────────────────────
    let populate = {
        let list_box = list_box.clone();
        let tab_view = tab_view.clone();
        let dialog = dialog.clone();
        move |query: &str| {
            // Remove existing rows.
            while let Some(child) = list_box.first_child() {
                list_box.remove(&child);
            }

            let entries = if query.is_empty() {
                all_entries(500)
            } else {
                self::search(query)
            };

            if entries.is_empty() {
                let label = gtk4::Label::new(Some("No history entries"));
                label.add_css_class("dim-label");
                label.set_margin_top(24);
                label.set_margin_bottom(24);
                list_box.append(&label);
                return;
            }

            for entry in &entries {
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(&entry.title))
                    .subtitle(glib::markup_escape_text(&entry.url))
                    .activatable(true)
                    .build();

                let visits_label =
                    gtk4::Label::new(Some(&format!("{}\u{00d7}", entry.visit_count)));
                visits_label.add_css_class("dim-label");
                row.add_suffix(&visits_label);

                let url = entry.url.clone();
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
    };

    // Initial population.
    populate("");

    // ── Search filtering ────────────────────────────────────────────
    let populate_for_search = populate.clone();
    search_entry.connect_search_changed(move |entry| {
        let query = entry.text().to_string();
        populate_for_search(&query);
    });

    // ── Clear all ───────────────────────────────────────────────────
    clear_btn.connect_clicked(clone!(
        #[weak]
        list_box,
        move |_| {
            clear_all();
            while let Some(child) = list_box.first_child() {
                list_box.remove(&child);
            }
            let label = gtk4::Label::new(Some("History cleared"));
            label.add_css_class("dim-label");
            label.set_margin_top(24);
            label.set_margin_bottom(24);
            list_box.append(&label);
        }
    ));

    dialog.present();
}
