// ─────────────────────────────────────────────────────────────────────
// window.rs  –  Next-gen browser window with vertical tabs & sidebar
// ─────────────────────────────────────────────────────────────────────
//
// Layout:  Everything lives in the sidebar.  The content area is a
//          clean, full-bleed WebView with only a thin progress bar.
//
//   ┌──────────────────────┬────────────────────────────────────────┐
//   │ [●][●][●] [🔍filter] │                                        │
//   │ ──────────────────── │                                        │
//   │ [<] [>] [↻]      [≡]│                                        │
//   │ [🔒 URL bar      ☆ ]│              WebView                   │
//   │ ──────────────────── │                or                      │
//   │ Workspaces ▾         │       Paned [ WebView | WebView ]      │
//   │ ──────────────────── │                                        │
//   │ Tab 1                │                                        │
//   │ Tab 2            ✕   │                                        │
//   │ Tab 3            ✕   │                                        │
//   │ ──────────────────── │                                        │
//   │ [+ New Tab]          │                                        │
//   └──────────────────────┴────────────────────────────────────────┘
//     Sidebar (resizable)           Content area (clean)
//
// Design:
//   - macOS-style traffic light dots at top-left of sidebar
//   - Tab search/filter next to traffic lights
//   - Navigation, URL bar (with bookmark), menu all in sidebar
//   - Content area has no header bar — just WebView + progress bar
//   - Sidebar is resizable via Paned drag handle
//   - Split view via inner gtk4::Paned
//   - Command palette (Ctrl+Shift+P)
// ─────────────────────────────────────────────────────────────────────

use glib::clone;
use gtk4::prelude::*;
use libadwaita as adw;
use webkit6::prelude::*;

use std::cell::RefCell;
use std::rc::Rc;

use crate::adblocker;
use crate::bookmarks;
use crate::browser_tab;
use crate::command_palette;
use crate::database;
use crate::downloads;
use crate::history;
use crate::passwords;
use crate::settings;
use crate::webview;

// ── Workspace data ──────────────────────────────────────────────────

#[derive(Clone, Debug)]
#[allow(dead_code)]
pub struct Workspace {
    pub id: i64,
    pub name: String,
    pub color: String,
    pub position: i32,
}

/// Load all workspaces from the database.
fn load_workspaces() -> Vec<Workspace> {
    database::with_db(|conn| {
        let mut stmt = conn
            .prepare("SELECT id, name, color, position FROM workspaces ORDER BY position")
            .expect("Failed to prepare workspace query");

        stmt.query_map([], |row| {
            Ok(Workspace {
                id: row.get(0)?,
                name: row.get(1)?,
                color: row.get(2)?,
                position: row.get(3)?,
            })
        })
        .expect("Failed to query workspaces")
        .filter_map(|r| r.ok())
        .collect()
    })
}

/// Add a new workspace to the database and return it.
fn create_workspace(name: &str, color: &str) -> Workspace {
    database::with_db(|conn| {
        let position: i64 = conn
            .query_row(
                "SELECT COALESCE(MAX(position), 0) + 1 FROM workspaces",
                [],
                |row| row.get(0),
            )
            .unwrap_or(0);

        conn.execute(
            "INSERT INTO workspaces (name, color, position) VALUES (?1, ?2, ?3)",
            rusqlite::params![name, color, position],
        )
        .expect("Failed to create workspace");

        let id = conn.last_insert_rowid();
        Workspace {
            id,
            name: name.to_string(),
            color: color.to_string(),
            position: position as i32,
        }
    })
}

/// After a workspace switch, select the first tab that belongs to the new
/// workspace. If `ws_id` is 0 ("All Workspaces"), select the first tab.
fn select_first_visible_tab(tab_list: &gtk4::ListBox, ws_id: i64) {
    let mut idx = 0;
    while let Some(row) = tab_list.row_at_index(idx) {
        if ws_id == 0 {
            // "All Workspaces" — select the first row.
            tab_list.select_row(Some(&row));
            return;
        }
        let row_ws: i64 = unsafe {
            row.data::<i64>("workspace-id")
                .map(|ptr| *ptr.as_ref())
                .unwrap_or(1)
        };
        if row_ws == ws_id {
            tab_list.select_row(Some(&row));
            return;
        }
        idx += 1;
    }
    // No matching tab found — deselect (shouldn't happen in practice).
    tab_list.unselect_all();
}

// ── CSS loader ──────────────────────────────────────────────────────

fn load_css() {
    let provider = gtk4::CssProvider::new();
    provider.load_from_string(include_str!("style.css"));

    gtk4::style_context_add_provider_for_display(
        &gtk4::gdk::Display::default().expect("Could not connect to a display"),
        &provider,
        gtk4::STYLE_PROVIDER_PRIORITY_APPLICATION,
    );
}

// ── Sidebar builder ─────────────────────────────────────────────────

/// Build the vertical tab sidebar widget with all browser controls.
///
/// The sidebar contains (top to bottom):
///   1. Title bar row: macOS traffic lights + tab search/filter entry
///   2. Navigation row: back, forward, reload, menu
///   3. URL bar: security icon + entry + bookmark button
///   4. Separator
///   5. Workspace selector
///   6. Tab list (scrollable)
///   7. Footer: new tab button
///
/// Returns `(sidebar_box, tab_list_box, workspace_label)`.
fn build_sidebar(
    tab_view: &adw::TabView,
    url_entry: &gtk4::Entry,
    progress_bar: &gtk4::ProgressBar,
    security_icon: &gtk4::Image,
    ucm: &webkit6::UserContentManager,
    back_btn: &gtk4::Button,
    forward_btn: &gtk4::Button,
    refresh_btn: &gtk4::Button,
    menu_btn: &gtk4::MenuButton,
    url_bar: &gtk4::Box,
    window_close_btn: &gtk4::Button,
    minimize_btn: &gtk4::Button,
    maximize_btn: &gtk4::Button,
    active_workspace_id: &Rc<RefCell<i64>>,
) -> (gtk4::Box, gtk4::ListBox, gtk4::Label) {
    let sidebar = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    sidebar.add_css_class("sidebar-container");
    sidebar.set_width_request(220);

    // ── 1. Title bar: traffic lights + tab search ───────────────────
    let title_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 8);
    title_row.add_css_class("sidebar-titlebar");

    // macOS-style traffic lights (close, minimize, maximize).
    let traffic_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 7);
    traffic_box.add_css_class("traffic-lights");
    traffic_box.set_valign(gtk4::Align::Center);
    traffic_box.append(window_close_btn);
    traffic_box.append(minimize_btn);
    traffic_box.append(maximize_btn);
    title_row.append(&traffic_box);

    // Tab search/filter entry (replaces the old "Wynn Browser" label).
    let tab_filter_entry = gtk4::SearchEntry::new();
    tab_filter_entry.set_placeholder_text(Some("Filter tabs\u{2026}"));
    tab_filter_entry.set_hexpand(true);
    tab_filter_entry.add_css_class("sidebar-tab-filter");
    title_row.append(&tab_filter_entry);

    sidebar.append(&title_row);

    // ── 2. Navigation row: back, forward, reload + menu ─────────────
    let nav_row = gtk4::Box::new(gtk4::Orientation::Horizontal, 2);
    nav_row.add_css_class("sidebar-nav-row");

    let nav_left = gtk4::Box::new(gtk4::Orientation::Horizontal, 0);
    nav_left.add_css_class("linked");
    nav_left.add_css_class("nav-buttons");
    nav_left.append(back_btn);
    nav_left.append(forward_btn);
    nav_left.append(refresh_btn);
    nav_left.set_hexpand(true);

    let nav_right = gtk4::Box::new(gtk4::Orientation::Horizontal, 2);
    nav_right.append(menu_btn.upcast_ref::<gtk4::Widget>());

    nav_row.append(&nav_left);
    nav_row.append(&nav_right);

    sidebar.append(&nav_row);

    // ── 3. URL bar (bookmark button is already appended inside) ─────
    sidebar.append(url_bar);

    // ── 4. Separator ────────────────────────────────────────────────
    let sep1 = gtk4::Separator::new(gtk4::Orientation::Horizontal);
    sidebar.append(&sep1);

    // ── 5. Workspace selector ───────────────────────────────────────
    let header_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    header_box.add_css_class("sidebar-header");

    let workspace_label = gtk4::Label::new(Some("Default"));
    workspace_label.set_hexpand(true);
    workspace_label.set_halign(gtk4::Align::Start);
    workspace_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);

    let ws_btn_content = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
    ws_btn_content.append(&workspace_label);
    let chevron = gtk4::Image::from_icon_name("pan-down-symbolic");
    chevron.add_css_class("workspace-chevron");
    ws_btn_content.append(&chevron);

    let workspace_btn = gtk4::Button::new();
    workspace_btn.set_child(Some(&ws_btn_content));
    workspace_btn.add_css_class("flat");
    workspace_btn.add_css_class("workspace-btn");
    workspace_btn.set_hexpand(true);
    workspace_btn.set_tooltip_text(Some("Switch workspace"));

    header_box.append(&workspace_btn);
    sidebar.append(&header_box);

    // Separator after workspace.
    let sep2 = gtk4::Separator::new(gtk4::Orientation::Horizontal);
    sidebar.append(&sep2);

    // ── 6. Tab list ─────────────────────────────────────────────────
    let tab_list = gtk4::ListBox::new();
    tab_list.set_selection_mode(gtk4::SelectionMode::Single);
    tab_list.add_css_class("sidebar-tab-list");

    let scrolled = gtk4::ScrolledWindow::new();
    scrolled.set_vexpand(true);
    scrolled.set_hscrollbar_policy(gtk4::PolicyType::Never);
    scrolled.set_child(Some(&tab_list));
    sidebar.append(&scrolled);

    // ── 6a. Tab filter logic ────────────────────────────────────────
    // Filters by both the text search query AND the active workspace.
    // Workspace ID 0 = "All Workspaces" (show everything).
    tab_list.set_filter_func(clone!(
        #[weak]
        tab_filter_entry,
        #[strong]
        active_workspace_id,
        #[upgrade_or]
        false,
        move |row| {
            // Workspace filter.
            let ws_id = *active_workspace_id.borrow();
            if ws_id > 0 {
                let row_ws: i64 = unsafe {
                    row.data::<i64>("workspace-id")
                        .map(|ptr| *ptr.as_ref())
                        .unwrap_or(1) // Default workspace = 1
                };
                if row_ws != ws_id {
                    return false;
                }
            }

            // Text search filter.
            let query = tab_filter_entry.text();
            if query.is_empty() {
                return true;
            }
            let query_lower = query.to_lowercase();
            // Check if the row's child label contains the query.
            if let Some(row_box) = row.child() {
                if let Some(row_box) = row_box.downcast_ref::<gtk4::Box>() {
                    // The title label is the second child (index 1).
                    if let Some(child) = row_box.first_child().and_then(|c| c.next_sibling()) {
                        if let Some(label) = child.downcast_ref::<gtk4::Label>() {
                            return label.text().to_lowercase().contains(&query_lower);
                        }
                    }
                }
            }
            true
        }
    ));

    tab_filter_entry.connect_search_changed(clone!(
        #[weak]
        tab_list,
        move |_| {
            tab_list.invalidate_filter();
        }
    ));

    // ── 7. Footer: new tab button ───────────────────────────────────
    let footer = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    footer.add_css_class("sidebar-footer");

    let sep3 = gtk4::Separator::new(gtk4::Orientation::Horizontal);
    footer.append(&sep3);

    let new_tab_btn = gtk4::Button::new();
    let btn_content = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    let plus_icon = gtk4::Image::from_icon_name("tab-new-symbolic");
    let plus_label = gtk4::Label::new(Some("New Tab"));
    btn_content.append(&plus_icon);
    btn_content.append(&plus_label);
    new_tab_btn.set_child(Some(&btn_content));
    new_tab_btn.add_css_class("flat");
    new_tab_btn.add_css_class("sidebar-new-tab-btn");
    new_tab_btn.set_tooltip_text(Some("New Tab (Ctrl+T)"));

    new_tab_btn.connect_clicked(clone!(
        #[weak]
        tab_view,
        #[weak]
        url_entry,
        #[weak]
        progress_bar,
        #[weak]
        security_icon,
        #[weak]
        tab_list,
        #[strong]
        ucm,
        #[strong]
        active_workspace_id,
        move |_| {
            let page =
                browser_tab::add_tab(&tab_view, &url_entry, &progress_bar, &security_icon, &ucm);
            let ws_id = *active_workspace_id.borrow();
            // If viewing "All Workspaces" (id=0), assign to default workspace (1).
            let assign_ws = if ws_id == 0 { 1 } else { ws_id };
            let row = create_sidebar_row(&page, &tab_view, &tab_list, assign_ws);
            tab_list.append(&row);
            tab_list.select_row(Some(&row));
        }
    ));

    footer.append(&new_tab_btn);
    sidebar.append(&footer);

    // ── Workspace button popover ────────────────────────────────────
    let workspace_popover = gtk4::Popover::new();
    let popover_box = gtk4::Box::new(gtk4::Orientation::Vertical, 4);
    popover_box.set_margin_start(6);
    popover_box.set_margin_end(6);
    popover_box.set_margin_top(6);
    popover_box.set_margin_bottom(6);

    // "All Workspaces" option (shows all tabs regardless of workspace).
    let all_ws_btn = gtk4::Button::with_label("All Workspaces");
    all_ws_btn.add_css_class("flat");
    all_ws_btn.connect_clicked(clone!(
        #[weak]
        workspace_popover,
        #[weak]
        workspace_label,
        #[weak]
        tab_list,
        #[strong]
        active_workspace_id,
        move |_| {
            *active_workspace_id.borrow_mut() = 0; // 0 = all
            workspace_label.set_label("All Workspaces");
            tab_list.invalidate_filter();
            select_first_visible_tab(&tab_list, 0);
            workspace_popover.popdown();
        }
    ));
    popover_box.append(&all_ws_btn);

    let all_sep = gtk4::Separator::new(gtk4::Orientation::Horizontal);
    popover_box.append(&all_sep);

    let workspaces = load_workspaces();
    for ws in &workspaces {
        let ws_btn = gtk4::Button::with_label(&ws.name);
        ws_btn.add_css_class("flat");
        let ws_name = ws.name.clone();
        let ws_id = ws.id;
        ws_btn.connect_clicked(clone!(
            #[weak]
            workspace_popover,
            #[weak]
            workspace_label,
            #[weak]
            tab_list,
            #[strong]
            active_workspace_id,
            move |_| {
                *active_workspace_id.borrow_mut() = ws_id;
                workspace_label.set_label(&ws_name);
                tab_list.invalidate_filter();
                select_first_visible_tab(&tab_list, ws_id);
                workspace_popover.popdown();
            }
        ));
        popover_box.append(&ws_btn);
    }

    // "New Workspace" button.
    let new_ws_sep = gtk4::Separator::new(gtk4::Orientation::Horizontal);
    popover_box.append(&new_ws_sep);

    let new_ws_btn = gtk4::Button::with_label("+ New Workspace");
    new_ws_btn.add_css_class("flat");
    new_ws_btn.connect_clicked(clone!(
        #[weak]
        workspace_popover,
        #[weak]
        workspace_label,
        #[weak]
        tab_list,
        #[strong]
        active_workspace_id,
        move |_| {
            let colors = [
                "#3584e4", "#33d17a", "#ff7800", "#e01b24", "#9141ac", "#f6d32d",
            ];
            let count = load_workspaces().len();
            let color = colors[count % colors.len()];
            let name = format!("Workspace {}", count + 1);
            let ws = create_workspace(&name, color);
            let new_id = ws.id;
            *active_workspace_id.borrow_mut() = new_id;
            workspace_label.set_label(&ws.name);
            tab_list.invalidate_filter();
            // New workspace has no tabs yet — deselect.
            select_first_visible_tab(&tab_list, new_id);
            workspace_popover.popdown();
        }
    ));
    popover_box.append(&new_ws_btn);

    workspace_popover.set_child(Some(&popover_box));
    workspace_popover.set_parent(&workspace_btn);
    workspace_btn.connect_clicked(clone!(
        #[weak]
        workspace_popover,
        move |_btn| {
            workspace_popover.popup();
        }
    ));

    (sidebar, tab_list, workspace_label)
}

// ── Sidebar tab row builder ─────────────────────────────────────────

/// Create a sidebar row widget for a tab page.
fn create_sidebar_row(
    page: &adw::TabPage,
    tab_view: &adw::TabView,
    _tab_list: &gtk4::ListBox,
    workspace_id: i64,
) -> gtk4::ListBoxRow {
    let row_box = gtk4::Box::new(gtk4::Orientation::Horizontal, 6);
    row_box.add_css_class("sidebar-tab-row");

    // Favicon.
    let favicon = gtk4::Image::from_icon_name("globe-symbolic");
    favicon.add_css_class("sidebar-tab-favicon");
    row_box.append(&favicon);

    // Title label.
    let title_label = gtk4::Label::new(Some(&page.title()));
    title_label.set_hexpand(true);
    title_label.set_halign(gtk4::Align::Start);
    title_label.set_ellipsize(gtk4::pango::EllipsizeMode::End);
    title_label.set_max_width_chars(25);
    title_label.add_css_class("sidebar-tab-title");
    row_box.append(&title_label);

    // Close button.
    let close_btn = gtk4::Button::from_icon_name("window-close-symbolic");
    close_btn.add_css_class("flat");
    close_btn.add_css_class("sidebar-tab-close");
    close_btn.set_valign(gtk4::Align::Center);
    close_btn.set_tooltip_text(Some("Close tab"));

    let page_close = page.clone();
    close_btn.connect_clicked(clone!(
        #[weak]
        tab_view,
        move |_| {
            tab_view.close_page(&page_close);
        }
    ));
    row_box.append(&close_btn);

    // Sync title changes.
    page.connect_title_notify(clone!(
        #[weak]
        title_label,
        move |p| {
            title_label.set_label(&p.title());
        }
    ));

    // Sync favicon (icon) changes.
    page.connect_icon_notify(clone!(
        #[weak]
        favicon,
        move |p| {
            if let Some(icon) = p.icon() {
                favicon.set_from_gicon(&icon);
            } else {
                favicon.set_icon_name(Some("globe-symbolic"));
            }
        }
    ));

    // Sync loading state (show spinner or favicon).
    page.connect_loading_notify(clone!(
        #[weak]
        favicon,
        move |p| {
            if p.is_loading() {
                favicon.set_icon_name(Some("content-loading-symbolic"));
                favicon.add_css_class("sidebar-tab-spinner");
            } else {
                favicon.remove_css_class("sidebar-tab-spinner");
                if let Some(icon) = p.icon() {
                    favicon.set_from_gicon(&icon);
                } else {
                    favicon.set_icon_name(Some("globe-symbolic"));
                }
            }
        }
    ));

    let list_row = gtk4::ListBoxRow::new();
    list_row.set_child(Some(&row_box));

    // Store the page pointer as data on the row for later retrieval.
    unsafe {
        list_row.set_data("tab-page-index", tab_view.page_position(page));
        list_row.set_data("workspace-id", workspace_id);
    }

    list_row
}

// ── Main build function ─────────────────────────────────────────────

/// Build the main browser window, wire up all signals, and present it.
pub fn build_window(app: &adw::Application) {
    // ── 0. Load custom CSS ──────────────────────────────────────────
    load_css();

    // ── 0a. Create shared UserContentManager for ad blocking ────────
    let adblock_enabled = adblocker::is_enabled();
    let ucm = adblocker::create_content_manager(adblock_enabled);

    // Inject privacy scripts (DNT, WebRTC leak prevention).
    webview::inject_privacy_scripts(&ucm);

    // ── 0b. Create shared download list ─────────────────────────────
    let download_list = downloads::new_download_list();

    // ── 0c. Split view state ────────────────────────────────────────
    let split_active = Rc::new(RefCell::new(false));

    // ── 1. Tab infrastructure ───────────────────────────────────────
    let tab_view = adw::TabView::new();

    // ── 2. Create all control widgets ───────────────────────────────
    // These will be passed to build_sidebar and placed there.

    // Navigation buttons.
    let back_btn = gtk4::Button::from_icon_name("go-previous-symbolic");
    back_btn.set_tooltip_text(Some("Back (Alt+\u{2190})"));
    back_btn.add_css_class("flat");

    let forward_btn = gtk4::Button::from_icon_name("go-next-symbolic");
    forward_btn.set_tooltip_text(Some("Forward (Alt+\u{2192})"));
    forward_btn.add_css_class("flat");

    let refresh_btn = gtk4::Button::from_icon_name("view-refresh-symbolic");
    refresh_btn.set_tooltip_text(Some("Reload (F5)"));
    refresh_btn.add_css_class("flat");

    // URL entry.
    let security_icon = gtk4::Image::from_icon_name("channel-insecure-symbolic");
    security_icon.add_css_class("security-icon");

    let url_entry = gtk4::Entry::new();
    url_entry.set_hexpand(true);
    url_entry.set_placeholder_text(Some("Search or enter address\u{2026}"));
    url_entry.set_input_purpose(gtk4::InputPurpose::Url);
    url_entry.add_css_class("url-entry");

    let url_bar = gtk4::Box::new(gtk4::Orientation::Horizontal, 4);
    url_bar.set_valign(gtk4::Align::Center);
    url_bar.add_css_class("url-bar");
    url_bar.append(&security_icon);
    url_bar.append(&url_entry);

    // Bookmark button — lives inside the URL bar on the right.
    let bookmark_btn = gtk4::Button::from_icon_name("non-starred-symbolic");
    bookmark_btn.set_tooltip_text(Some("Bookmark (Ctrl+D)"));
    bookmark_btn.add_css_class("flat");
    bookmark_btn.add_css_class("url-bookmark-btn");
    url_bar.append(&bookmark_btn);

    // Menu button.
    let menu_btn = gtk4::MenuButton::new();
    menu_btn.set_icon_name("open-menu-symbolic");
    menu_btn.set_tooltip_text(Some("Menu"));
    menu_btn.add_css_class("flat");

    let menu = gio::Menu::new();
    menu.append(Some("History"), Some("win.show-history"));
    menu.append(Some("Bookmarks"), Some("win.show-bookmarks"));
    menu.append(Some("Downloads"), Some("win.show-downloads"));
    menu.append(Some("Passwords"), Some("win.show-passwords"));

    let view_section = gio::Menu::new();
    view_section.append(Some("Toggle Split View"), Some("win.toggle-split-view"));
    view_section.append(Some("Toggle Sidebar"), Some("win.toggle-sidebar"));
    view_section.append(Some("Command Palette"), Some("win.command-palette"));
    menu.append_section(Some("View"), &view_section);

    let tools_section = gio::Menu::new();
    tools_section.append(Some("Refresh Blocklist"), Some("win.refresh-blocklist"));
    tools_section.append(Some("Settings"), Some("win.show-settings"));
    menu.append_section(Some("Tools"), &tools_section);

    menu_btn.set_menu_model(Some(&menu));

    // Window control buttons (macOS-style traffic lights).
    let window_close_btn = gtk4::Button::new();
    window_close_btn.add_css_class("traffic-dot");
    window_close_btn.add_css_class("traffic-close");
    window_close_btn.set_tooltip_text(Some("Close Window"));
    let close_dot = gtk4::DrawingArea::new();
    close_dot.set_content_width(12);
    close_dot.set_content_height(12);
    close_dot.set_draw_func(|_area, cr, w, h| {
        cr.arc(
            w as f64 / 2.0,
            h as f64 / 2.0,
            5.5,
            0.0,
            2.0 * std::f64::consts::PI,
        );
        cr.set_source_rgb(1.0, 0.373, 0.341); // #ff5f57
        let _ = cr.fill();
    });
    window_close_btn.set_child(Some(&close_dot));

    let minimize_btn = gtk4::Button::new();
    minimize_btn.add_css_class("traffic-dot");
    minimize_btn.add_css_class("traffic-minimize");
    minimize_btn.set_tooltip_text(Some("Minimize"));
    let min_dot = gtk4::DrawingArea::new();
    min_dot.set_content_width(12);
    min_dot.set_content_height(12);
    min_dot.set_draw_func(|_area, cr, w, h| {
        cr.arc(
            w as f64 / 2.0,
            h as f64 / 2.0,
            5.5,
            0.0,
            2.0 * std::f64::consts::PI,
        );
        cr.set_source_rgb(1.0, 0.741, 0.180); // #ffbd2e
        let _ = cr.fill();
    });
    minimize_btn.set_child(Some(&min_dot));

    let maximize_btn = gtk4::Button::new();
    maximize_btn.add_css_class("traffic-dot");
    maximize_btn.add_css_class("traffic-maximize");
    maximize_btn.set_tooltip_text(Some("Maximize"));
    let max_dot = gtk4::DrawingArea::new();
    max_dot.set_content_width(12);
    max_dot.set_content_height(12);
    max_dot.set_draw_func(|_area, cr, w, h| {
        cr.arc(
            w as f64 / 2.0,
            h as f64 / 2.0,
            5.5,
            0.0,
            2.0 * std::f64::consts::PI,
        );
        cr.set_source_rgb(0.157, 0.784, 0.325); // #28c840
        let _ = cr.fill();
    });
    maximize_btn.set_child(Some(&max_dot));

    // Loading progress bar.
    let progress_bar = gtk4::ProgressBar::new();
    progress_bar.add_css_class("osd");
    progress_bar.add_css_class("load-progress");
    progress_bar.set_visible(false);

    // ── 2c. Active workspace tracking ─────────────────────────────
    let active_workspace_id = Rc::new(RefCell::new(1i64)); // Default workspace = 1

    // ── 3. Build the sidebar with all controls ──────────────────────
    let (sidebar, tab_list, _workspace_label) = build_sidebar(
        &tab_view,
        &url_entry,
        &progress_bar,
        &security_icon,
        &ucm,
        &back_btn,
        &forward_btn,
        &refresh_btn,
        &menu_btn,
        &url_bar,
        &window_close_btn,
        &minimize_btn,
        &maximize_btn,
        &active_workspace_id,
    );

    // ── 4. Build the content area (clean — no header bar) ───────────
    // Paned for split view.
    let paned = gtk4::Paned::new(gtk4::Orientation::Horizontal);
    paned.set_wide_handle(true);

    let content_stack = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    content_stack.set_vexpand(true);
    content_stack.set_hexpand(true);
    content_stack.append(&tab_view);

    paned.set_start_child(Some(&content_stack));

    // Split view placeholder (empty initially).
    let split_placeholder = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    split_placeholder.set_visible(false);
    paned.set_end_child(Some(&split_placeholder));
    paned.set_position(640);

    // Toast overlay wraps everything.
    let toast_overlay = adw::ToastOverlay::new();
    toast_overlay.set_child(Some(&paned));

    // Content container: thin header bar (for CSD drag) + progress + webview.
    let content_header = adw::HeaderBar::new();
    content_header.add_css_class("content-header");
    // Remove all decorations — the close button is in the sidebar.
    content_header.set_show_title(false);
    content_header.set_decoration_layout(Some(""));

    let content_view = adw::ToolbarView::new();
    content_view.add_top_bar(&content_header);
    content_view.add_top_bar(&progress_bar);
    content_view.set_content(Some(&toast_overlay));

    // ── 5. Resizable sidebar via Paned ────────────────────────────────
    let sidebar_paned = gtk4::Paned::new(gtk4::Orientation::Horizontal);
    sidebar_paned.set_start_child(Some(&sidebar));
    sidebar_paned.set_end_child(Some(&content_view));
    sidebar_paned.set_position(280);
    sidebar_paned.set_shrink_start_child(false);
    sidebar_paned.set_shrink_end_child(false);
    sidebar_paned.set_resize_start_child(false);
    sidebar_paned.set_resize_end_child(true);
    sidebar_paned.set_wide_handle(false);
    sidebar_paned.add_css_class("sidebar-paned");

    // ── 5b. Root overlay (for command palette, etc.) ───────────────
    let root_overlay = gtk4::Overlay::new();
    root_overlay.set_child(Some(&sidebar_paned));

    // ── 6. Create the application window ────────────────────────────
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("Wynn Browser")
        .default_width(1400)
        .default_height(850)
        .content(&root_overlay)
        .build();

    // Register the JS-based password capture handler (replaces the old
    // connect_submit_form approach that caused SIGSEGV on Google login).
    browser_tab::register_password_capture_handler(&ucm, &window);

    // Wire the close button to actually close the window.
    window_close_btn.connect_clicked(clone!(
        #[weak]
        window,
        move |_| {
            window.close();
        }
    ));

    // Wire the minimize button.
    minimize_btn.connect_clicked(clone!(
        #[weak]
        window,
        move |_| {
            window.minimize();
        }
    ));

    // Wire the maximize button.
    maximize_btn.connect_clicked(clone!(
        #[weak]
        window,
        move |_| {
            if window.is_maximized() {
                window.unmaximize();
            } else {
                window.maximize();
            }
        }
    ));

    // ── 6a. Create the first tab ────────────────────────────────────
    let first_page =
        browser_tab::add_tab(&tab_view, &url_entry, &progress_bar, &security_icon, &ucm);

    let first_row = create_sidebar_row(
        &first_page,
        &tab_view,
        &tab_list,
        *active_workspace_id.borrow(),
    );
    tab_list.append(&first_row);
    tab_list.select_row(Some(&first_row));

    // ── 6b. Setup privacy on NetworkSession ─────────────────────────
    if let Some(wv) = browser_tab::active_webview(&tab_view) {
        if let Some(session) = wv.network_session() {
            webview::setup_privacy_on_session(&session);
            downloads::setup_download_handler(&session, &download_list);
        }
    }

    // ── 11. Signal: URL entry activated ─────────────────────────────
    url_entry.connect_activate(clone!(
        #[weak]
        tab_view,
        move |entry| {
            let text = entry.text();
            if !text.is_empty() {
                if let Some(wv) = browser_tab::active_webview(&tab_view) {
                    crate::webview::navigate_to(&wv, &text);
                }
            }
        }
    ));

    // ── 11a. Signal: URL entry focus → select all ───────────────────
    let focus_controller = gtk4::EventControllerFocus::new();
    focus_controller.connect_enter(clone!(
        #[weak]
        url_entry,
        move |_| {
            glib::idle_add_local_once(clone!(
                #[weak]
                url_entry,
                move || {
                    url_entry.select_region(0, -1);
                }
            ));
        }
    ));
    url_entry.add_controller(focus_controller);

    // ── 12. Signal: Back button ─────────────────────────────────────
    back_btn.connect_clicked(clone!(
        #[weak]
        tab_view,
        move |_| {
            if let Some(wv) = browser_tab::active_webview(&tab_view) {
                if wv.can_go_back() {
                    wv.go_back();
                }
            }
        }
    ));

    // ── 13. Signal: Forward button ──────────────────────────────────
    forward_btn.connect_clicked(clone!(
        #[weak]
        tab_view,
        move |_| {
            if let Some(wv) = browser_tab::active_webview(&tab_view) {
                if wv.can_go_forward() {
                    wv.go_forward();
                }
            }
        }
    ));

    // ── 14. Signal: Reload/Stop button ──────────────────────────────
    refresh_btn.connect_clicked(clone!(
        #[weak]
        tab_view,
        move |btn| {
            if let Some(wv) = browser_tab::active_webview(&tab_view) {
                if wv.is_loading() {
                    wv.stop_loading();
                    btn.set_icon_name("view-refresh-symbolic");
                    btn.set_tooltip_text(Some("Reload (F5)"));
                } else {
                    wv.reload();
                }
            }
        }
    ));

    // ── 15. Signal: Bookmark button ─────────────────────────────────
    bookmark_btn.connect_clicked(clone!(
        #[weak]
        tab_view,
        move |btn| {
            if let Some(wv) = browser_tab::active_webview(&tab_view) {
                let url = wv.uri().map(|u| u.to_string()).unwrap_or_default();
                let title = wv.title().map(|t| t.to_string()).unwrap_or_default();
                let is_bookmarked = bookmarks::toggle_bookmark(&url, &title);
                if is_bookmarked {
                    btn.set_icon_name("starred-symbolic");
                    btn.set_tooltip_text(Some("Remove Bookmark (Ctrl+D)"));
                } else {
                    btn.set_icon_name("non-starred-symbolic");
                    btn.set_tooltip_text(Some("Bookmark (Ctrl+D)"));
                }
            }
        }
    ));

    // ── 16. (Sidebar toggle is now via menu action, no standalone button) ──

    // ── 17. Signal: Tab view page selected → sync sidebar ───────────
    tab_view.connect_selected_page_notify(clone!(
        #[weak]
        url_entry,
        #[weak]
        window,
        #[weak]
        progress_bar,
        #[weak]
        refresh_btn,
        #[weak]
        security_icon,
        #[weak]
        bookmark_btn,
        #[weak]
        tab_list,
        #[weak]
        tab_view,
        move |tv| {
            if let Some(wv) = browser_tab::active_webview(tv) {
                // Sync the address bar.
                if let Some(uri) = wv.uri() {
                    url_entry.set_text(&uri);
                    browser_tab::update_security_icon(&security_icon, &uri);

                    if bookmarks::is_bookmarked(&uri) {
                        bookmark_btn.set_icon_name("starred-symbolic");
                    } else {
                        bookmark_btn.set_icon_name("non-starred-symbolic");
                    }
                } else {
                    url_entry.set_text("");
                    security_icon.set_icon_name(Some("system-search-symbolic"));
                    security_icon.remove_css_class("security-secure");
                    security_icon.remove_css_class("security-insecure");
                    bookmark_btn.set_icon_name("non-starred-symbolic");
                }

                // Sync the window title.
                let title = wv
                    .title()
                    .filter(|t| !t.is_empty())
                    .map(|t| format!("{t} \u{2014} Wynn Browser"))
                    .unwrap_or_else(|| "Wynn Browser".to_string());
                window.set_title(Some(&title));

                // Sync the progress bar.
                if wv.is_loading() {
                    let progress = wv.estimated_load_progress();
                    progress_bar.set_fraction(progress);
                    progress_bar.set_visible(true);
                    refresh_btn.set_icon_name("process-stop-symbolic");
                    refresh_btn.set_tooltip_text(Some("Stop"));
                } else {
                    progress_bar.set_visible(false);
                    progress_bar.set_fraction(0.0);
                    refresh_btn.set_icon_name("view-refresh-symbolic");
                    refresh_btn.set_tooltip_text(Some("Reload (F5)"));
                }

                // Sync sidebar selection.
                if let Some(selected_page) = tv.selected_page() {
                    let page_pos = tab_view.page_position(&selected_page);
                    if let Some(row) = tab_list.row_at_index(page_pos) {
                        tab_list.select_row(Some(&row));
                    }
                }
            }
        }
    ));

    // ── 17a. Signal: Sidebar row selected → sync TabView ────────────
    tab_list.connect_row_selected(clone!(
        #[weak]
        tab_view,
        move |_, row| {
            if let Some(row) = row {
                let idx = row.index();
                if idx >= 0 {
                    let n = tab_view.n_pages();
                    if idx < n {
                        let page = tab_view.nth_page(idx);
                        tab_view.set_selected_page(&page);
                    }
                }
            }
        }
    ));

    // ── 18. Signal: Tab close → remove sidebar row ──────────────────
    tab_view.connect_close_page(clone!(
        #[weak]
        window,
        #[weak]
        tab_list,
        #[weak]
        tab_view,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |tv, page| {
            // Find and remove the corresponding sidebar row.
            let pos = tab_view.page_position(page);
            if let Some(row) = tab_list.row_at_index(pos) {
                tab_list.remove(&row);
            }

            tv.close_page_finish(page, true);

            if tv.n_pages() == 0 {
                window.close();
            }

            glib::Propagation::Stop
        }
    ));

    // ── 19. Keyboard shortcuts via GAction ──────────────────────────

    // Action: new-tab (Ctrl+T)
    let action_new_tab = gio::SimpleAction::new("new-tab", None);
    action_new_tab.connect_activate(clone!(
        #[weak]
        tab_view,
        #[weak]
        url_entry,
        #[weak]
        progress_bar,
        #[weak]
        security_icon,
        #[weak]
        tab_list,
        #[strong]
        ucm,
        #[strong]
        active_workspace_id,
        move |_, _| {
            let page =
                browser_tab::add_tab(&tab_view, &url_entry, &progress_bar, &security_icon, &ucm);
            let ws_id = *active_workspace_id.borrow();
            // If viewing "All Workspaces" (id=0), assign to default workspace (1).
            let assign_ws = if ws_id == 0 { 1 } else { ws_id };
            let row = create_sidebar_row(&page, &tab_view, &tab_list, assign_ws);
            tab_list.append(&row);
            tab_list.select_row(Some(&row));
        }
    ));
    window.add_action(&action_new_tab);

    // Action: close-tab (Ctrl+W)
    let action_close_tab = gio::SimpleAction::new("close-tab", None);
    action_close_tab.connect_activate(clone!(
        #[weak]
        tab_view,
        move |_, _| {
            if let Some(page) = tab_view.selected_page() {
                tab_view.close_page(&page);
            }
        }
    ));
    window.add_action(&action_close_tab);

    // Action: focus-url (Ctrl+L)
    let action_focus_url = gio::SimpleAction::new("focus-url", None);
    action_focus_url.connect_activate(clone!(
        #[weak]
        url_entry,
        move |_, _| {
            url_entry.grab_focus();
            url_entry.select_region(0, -1);
        }
    ));
    window.add_action(&action_focus_url);

    // Action: reload (F5 / Ctrl+R)
    let action_reload = gio::SimpleAction::new("reload", None);
    action_reload.connect_activate(clone!(
        #[weak]
        tab_view,
        move |_, _| {
            if let Some(wv) = browser_tab::active_webview(&tab_view) {
                if wv.is_loading() {
                    wv.stop_loading();
                } else {
                    wv.reload();
                }
            }
        }
    ));
    window.add_action(&action_reload);

    // Action: go-back (Alt+Left)
    let action_back = gio::SimpleAction::new("go-back", None);
    action_back.connect_activate(clone!(
        #[weak]
        tab_view,
        move |_, _| {
            if let Some(wv) = browser_tab::active_webview(&tab_view) {
                if wv.can_go_back() {
                    wv.go_back();
                }
            }
        }
    ));
    window.add_action(&action_back);

    // Action: go-forward (Alt+Right)
    let action_forward = gio::SimpleAction::new("go-forward", None);
    action_forward.connect_activate(clone!(
        #[weak]
        tab_view,
        move |_, _| {
            if let Some(wv) = browser_tab::active_webview(&tab_view) {
                if wv.can_go_forward() {
                    wv.go_forward();
                }
            }
        }
    ));
    window.add_action(&action_forward);

    // Action: toggle-bookmark (Ctrl+D)
    let action_bookmark = gio::SimpleAction::new("toggle-bookmark", None);
    action_bookmark.connect_activate(clone!(
        #[weak]
        tab_view,
        #[weak]
        bookmark_btn,
        move |_, _| {
            if let Some(wv) = browser_tab::active_webview(&tab_view) {
                let url = wv.uri().map(|u| u.to_string()).unwrap_or_default();
                let title = wv.title().map(|t| t.to_string()).unwrap_or_default();
                let is_bookmarked = bookmarks::toggle_bookmark(&url, &title);
                if is_bookmarked {
                    bookmark_btn.set_icon_name("starred-symbolic");
                } else {
                    bookmark_btn.set_icon_name("non-starred-symbolic");
                }
            }
        }
    ));
    window.add_action(&action_bookmark);

    // Action: show-history (Ctrl+H)
    let action_history = gio::SimpleAction::new("show-history", None);
    action_history.connect_activate(clone!(
        #[weak]
        window,
        #[weak]
        tab_view,
        move |_, _| {
            history::show_history_dialog(&window, &tab_view);
        }
    ));
    window.add_action(&action_history);

    // Action: show-bookmarks
    let action_bookmarks = gio::SimpleAction::new("show-bookmarks", None);
    action_bookmarks.connect_activate(clone!(
        #[weak]
        window,
        #[weak]
        tab_view,
        move |_, _| {
            bookmarks::show_bookmarks_dialog(&window, &tab_view);
        }
    ));
    window.add_action(&action_bookmarks);

    // Action: show-downloads (Ctrl+J)
    let action_downloads = gio::SimpleAction::new("show-downloads", None);
    action_downloads.connect_activate(clone!(
        #[weak]
        window,
        #[strong]
        download_list,
        move |_, _| {
            downloads::show_downloads_dialog(&window, &download_list);
        }
    ));
    window.add_action(&action_downloads);

    // Action: show-settings (Ctrl+Comma)
    let action_settings = gio::SimpleAction::new("show-settings", None);
    action_settings.connect_activate(clone!(
        #[weak]
        window,
        #[strong]
        ucm,
        move |_, _| {
            settings::show_settings(&window, &ucm);
        }
    ));
    window.add_action(&action_settings);

    // Action: show-passwords
    let action_passwords = gio::SimpleAction::new("show-passwords", None);
    action_passwords.connect_activate(clone!(
        #[weak]
        window,
        move |_, _| {
            passwords::show_passwords_dialog(&window);
        }
    ));
    window.add_action(&action_passwords);

    // Action: toggle-sidebar (Ctrl+\)
    let action_sidebar = gio::SimpleAction::new("toggle-sidebar", None);
    let sidebar_visible = Rc::new(RefCell::new(true));
    let saved_pos = Rc::new(RefCell::new(280i32));
    action_sidebar.connect_activate(clone!(
        #[weak]
        sidebar_paned,
        #[weak]
        sidebar,
        #[strong]
        sidebar_visible,
        #[strong]
        saved_pos,
        move |_, _| {
            let mut visible = sidebar_visible.borrow_mut();
            if *visible {
                *saved_pos.borrow_mut() = sidebar_paned.position();
                sidebar.set_visible(false);
                sidebar_paned.set_position(0);
                *visible = false;
            } else {
                sidebar.set_visible(true);
                sidebar_paned.set_position(*saved_pos.borrow());
                *visible = true;
            }
        }
    ));
    window.add_action(&action_sidebar);

    // Action: command-palette (Ctrl+Shift+P)
    let action_palette = gio::SimpleAction::new("command-palette", None);
    action_palette.connect_activate(clone!(
        #[weak]
        window,
        #[weak]
        root_overlay,
        move |_, _| {
            command_palette::show_command_palette(&window, &root_overlay);
        }
    ));
    window.add_action(&action_palette);

    // Action: toggle-split-view (Ctrl+Shift+E)
    let action_split = gio::SimpleAction::new("toggle-split-view", None);
    action_split.connect_activate(clone!(
        #[weak]
        split_placeholder,
        #[weak]
        paned,
        #[weak]
        toast_overlay,
        #[strong]
        ucm,
        #[strong]
        split_active,
        move |_, _| {
            let mut active = split_active.borrow_mut();
            if *active {
                // Deactivate split view.
                *active = false;
                // Remove the split webview.
                while let Some(child) = split_placeholder.first_child() {
                    split_placeholder.remove(&child);
                }
                split_placeholder.set_visible(false);

                toast_overlay.add_toast(adw::Toast::new("Split view closed"));
            } else {
                // Activate split view.
                *active = true;
                let split_wv = webview::create_webview(&ucm);
                let home = webview::home_url();
                webview::navigate_to(&split_wv, &home);

                split_placeholder.append(&split_wv);
                split_placeholder.set_visible(true);

                // Set paned position to half.
                let width = paned.width();
                if width > 0 {
                    paned.set_position(width / 2);
                }

                toast_overlay.add_toast(adw::Toast::new("Split view opened"));
            }
        }
    ));
    window.add_action(&action_split);

    // Action: refresh-blocklist
    let action_refresh_bl = gio::SimpleAction::new("refresh-blocklist", None);
    action_refresh_bl.connect_activate(clone!(
        #[weak]
        toast_overlay,
        #[strong]
        ucm,
        move |_, _| {
            toast_overlay.add_toast(adw::Toast::new("Refreshing blocklist\u{2026}"));

            // Run refresh in a background thread to avoid blocking UI.
            let toast = toast_overlay.clone();
            let ucm_inner = ucm.clone();
            glib::idle_add_local_once(move || {
                let count = adblocker::refresh_blocklist();
                if count > 0 {
                    // Recompile the native WebKit content filter with new domains.
                    adblocker::recompile_native_filter(&ucm_inner);
                    toast.add_toast(adw::Toast::new(&format!(
                        "Blocklist updated: {} domains blocked",
                        count
                    )));
                } else {
                    toast.add_toast(adw::Toast::new("Failed to refresh blocklist"));
                }
            });
        }
    ));
    window.add_action(&action_refresh_bl);

    // Action: go-home (Alt+Home)
    let action_home = gio::SimpleAction::new("go-home", None);
    action_home.connect_activate(clone!(
        #[weak]
        tab_view,
        move |_, _| {
            if let Some(wv) = browser_tab::active_webview(&tab_view) {
                let home = webview::home_url();
                webview::navigate_to(&wv, &home);
            }
        }
    ));
    window.add_action(&action_home);

    // Action: stop-loading
    let action_stop = gio::SimpleAction::new("stop-loading", None);
    action_stop.connect_activate(clone!(
        #[weak]
        tab_view,
        move |_, _| {
            if let Some(wv) = browser_tab::active_webview(&tab_view) {
                wv.stop_loading();
            }
        }
    ));
    window.add_action(&action_stop);

    // Action: clear-history
    let action_clear_hist = gio::SimpleAction::new("clear-history", None);
    action_clear_hist.connect_activate(clone!(
        #[weak]
        toast_overlay,
        move |_, _| {
            history::clear_all();
            toast_overlay.add_toast(adw::Toast::new("Browsing history cleared"));
        }
    ));
    window.add_action(&action_clear_hist);

    // Action: toggle-adblock
    let action_toggle_adblock = gio::SimpleAction::new("toggle-adblock", None);
    action_toggle_adblock.connect_activate(clone!(
        #[weak]
        toast_overlay,
        #[strong]
        ucm,
        move |_, _| {
            let currently_enabled = adblocker::is_enabled();
            let new_state = !currently_enabled;
            adblocker::set_enabled(new_state);
            if new_state {
                adblocker::load_rules(&ucm);
                toast_overlay.add_toast(adw::Toast::new("Ad blocker enabled"));
            } else {
                adblocker::clear_rules(&ucm);
                toast_overlay.add_toast(adw::Toast::new("Ad blocker disabled"));
            }
        }
    ));
    window.add_action(&action_toggle_adblock);

    // Action: zoom-in (Ctrl+=)
    let action_zoom_in = gio::SimpleAction::new("zoom-in", None);
    action_zoom_in.connect_activate(clone!(
        #[weak]
        tab_view,
        move |_, _| {
            if let Some(wv) = browser_tab::active_webview(&tab_view) {
                let level = wv.zoom_level();
                wv.set_zoom_level((level + 0.1).min(3.0));
            }
        }
    ));
    window.add_action(&action_zoom_in);

    // Action: zoom-out (Ctrl+-)
    let action_zoom_out = gio::SimpleAction::new("zoom-out", None);
    action_zoom_out.connect_activate(clone!(
        #[weak]
        tab_view,
        move |_, _| {
            if let Some(wv) = browser_tab::active_webview(&tab_view) {
                let level = wv.zoom_level();
                wv.set_zoom_level((level - 0.1).max(0.3));
            }
        }
    ));
    window.add_action(&action_zoom_out);

    // Action: zoom-reset (Ctrl+0)
    let action_zoom_reset = gio::SimpleAction::new("zoom-reset", None);
    action_zoom_reset.connect_activate(clone!(
        #[weak]
        tab_view,
        move |_, _| {
            if let Some(wv) = browser_tab::active_webview(&tab_view) {
                wv.set_zoom_level(1.0);
            }
        }
    ));
    window.add_action(&action_zoom_reset);

    // Action: copy-url
    let action_copy_url = gio::SimpleAction::new("copy-url", None);
    action_copy_url.connect_activate(clone!(
        #[weak]
        tab_view,
        #[weak]
        toast_overlay,
        move |_, _| {
            if let Some(wv) = browser_tab::active_webview(&tab_view) {
                if let Some(uri) = wv.uri() {
                    if let Some(display) = gtk4::gdk::Display::default() {
                        let clipboard = display.clipboard();
                        clipboard.set_text(&uri);
                        toast_overlay.add_toast(adw::Toast::new("URL copied to clipboard"));
                    }
                }
            }
        }
    ));
    window.add_action(&action_copy_url);

    // Action: fullscreen (F11)
    let action_fullscreen = gio::SimpleAction::new("toggle-fullscreen", None);
    action_fullscreen.connect_activate(clone!(
        #[weak]
        window,
        move |_, _| {
            if window.is_fullscreen() {
                window.unfullscreen();
            } else {
                window.fullscreen();
            }
        }
    ));
    window.add_action(&action_fullscreen);

    // Action: print-page (Ctrl+P)
    let action_print = gio::SimpleAction::new("print-page", None);
    action_print.connect_activate(clone!(
        #[weak]
        tab_view,
        #[weak]
        window,
        move |_, _| {
            if let Some(wv) = browser_tab::active_webview(&tab_view) {
                let print_op = webkit6::PrintOperation::new(&wv);
                print_op.run_dialog(Some(&window));
            }
        }
    ));
    window.add_action(&action_print);

    // Action: view-source
    let action_view_source = gio::SimpleAction::new("view-source", None);
    action_view_source.connect_activate(clone!(
        #[weak]
        tab_view,
        move |_, _| {
            if let Some(wv) = browser_tab::active_webview(&tab_view) {
                if let Some(uri) = wv.uri() {
                    let src_url = if uri.starts_with("view-source:") {
                        uri.to_string()
                    } else {
                        format!("view-source:{uri}")
                    };
                    webview::navigate_to(&wv, &src_url);
                }
            }
        }
    ));
    window.add_action(&action_view_source);

    // Action: open-devtools
    let action_devtools = gio::SimpleAction::new("open-devtools", None);
    action_devtools.connect_activate(clone!(
        #[weak]
        tab_view,
        move |_, _| {
            if let Some(wv) = browser_tab::active_webview(&tab_view) {
                if let Some(inspector) = wv.inspector() {
                    inspector.show();
                }
            }
        }
    ));
    window.add_action(&action_devtools);

    // Action: next-tab (Ctrl+Tab)
    let action_next_tab = gio::SimpleAction::new("next-tab", None);
    action_next_tab.connect_activate(clone!(
        #[weak]
        tab_view,
        move |_, _| {
            let n = tab_view.n_pages();
            if n > 1 {
                if let Some(page) = tab_view.selected_page() {
                    let pos = tab_view.page_position(&page);
                    let next = if pos + 1 < n { pos + 1 } else { 0 };
                    let next_page = tab_view.nth_page(next);
                    tab_view.set_selected_page(&next_page);
                }
            }
        }
    ));
    window.add_action(&action_next_tab);

    // Action: prev-tab (Ctrl+Shift+Tab)
    let action_prev_tab = gio::SimpleAction::new("prev-tab", None);
    action_prev_tab.connect_activate(clone!(
        #[weak]
        tab_view,
        move |_, _| {
            let n = tab_view.n_pages();
            if n > 1 {
                if let Some(page) = tab_view.selected_page() {
                    let pos = tab_view.page_position(&page);
                    let prev = if pos > 0 { pos - 1 } else { n - 1 };
                    let prev_page = tab_view.nth_page(prev);
                    tab_view.set_selected_page(&prev_page);
                }
            }
        }
    ));
    window.add_action(&action_prev_tab);

    // ── 20. Bind keyboard accelerators ──────────────────────────────
    app.set_accels_for_action("win.new-tab", &["<Ctrl>t"]);
    app.set_accels_for_action("win.close-tab", &["<Ctrl>w"]);
    app.set_accels_for_action("win.focus-url", &["<Ctrl>l"]);
    app.set_accels_for_action("win.reload", &["F5", "<Ctrl>r"]);
    app.set_accels_for_action("win.go-back", &["<Alt>Left"]);
    app.set_accels_for_action("win.go-forward", &["<Alt>Right"]);
    app.set_accels_for_action("win.toggle-bookmark", &["<Ctrl>d"]);
    app.set_accels_for_action("win.show-history", &["<Ctrl>h"]);
    app.set_accels_for_action("win.show-downloads", &["<Ctrl>j"]);
    app.set_accels_for_action("win.show-settings", &["<Ctrl>comma"]);
    app.set_accels_for_action("win.toggle-sidebar", &["<Ctrl>backslash"]);
    app.set_accels_for_action("win.command-palette", &["<Ctrl><Shift>p"]);
    app.set_accels_for_action("win.toggle-split-view", &["<Ctrl><Shift>e"]);
    app.set_accels_for_action("win.go-home", &["<Alt>Home"]);
    app.set_accels_for_action("win.zoom-in", &["<Ctrl>equal", "<Ctrl>plus"]);
    app.set_accels_for_action("win.zoom-out", &["<Ctrl>minus"]);
    app.set_accels_for_action("win.zoom-reset", &["<Ctrl>0"]);
    app.set_accels_for_action("win.toggle-fullscreen", &["F11"]);
    app.set_accels_for_action("win.print-page", &["<Ctrl>p"]);
    app.set_accels_for_action("win.open-devtools", &["F12"]);
    app.set_accels_for_action("win.next-tab", &["<Ctrl>Tab"]);
    app.set_accels_for_action("win.prev-tab", &["<Ctrl><Shift>Tab"]);
    app.set_accels_for_action("win.copy-url", &["<Ctrl><Shift>c"]);

    // ── 21. Show the window ─────────────────────────────────────────
    window.present();

    // ── 22. Show a toast with blocklist status ──────────────────────
    let domain_count = adblocker::blocklist_domain_count();
    if domain_count > 0 {
        toast_overlay.add_toast(adw::Toast::new(&format!(
            "Ad blocker active: {} domains blocked",
            domain_count
        )));
    }
}
