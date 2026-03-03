// ─────────────────────────────────────────────────────────────────────
// command_palette.rs  –  VS Code-style command palette for Wynn Browser
// ─────────────────────────────────────────────────────────────────────
//
// An in-window overlay triggered by Ctrl+Shift+P that shows a search
// entry and a categorised, fuzzy-scored list of all available browser
// commands.  Features:
//
//   • Rendered as a centered overlay inside the main window (not a
//     separate window / dialog)
//   • Translucent backdrop that dismisses on click
//   • Category-based grouping (Navigation, Tabs, View, Privacy, Tools)
//   • Fuzzy match scoring (exact > prefix > substring > fuzzy)
//   • Symbolic icons per command
//   • Recently-used tracking (in-memory, shown when query empty)
//   • Result count label
//   • Keyboard navigation keeps focus in search entry
//
// Public API:
//
//   show_command_palette(window, overlay) – present the palette.
// ─────────────────────────────────────────────────────────────────────

use glib::clone;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

// ─── Data types ─────────────────────────────────────────────────────

/// Categories for organising commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CommandCategory {
    Navigation,
    Tabs,
    View,
    PrivacySecurity,
    Tools,
}

impl CommandCategory {
    pub fn label(self) -> &'static str {
        match self {
            Self::Navigation => "Navigation",
            Self::Tabs => "Tabs",
            Self::View => "View",
            Self::PrivacySecurity => "Privacy & Security",
            Self::Tools => "Tools",
        }
    }

    /// Order in which categories appear.
    fn sort_key(self) -> u8 {
        match self {
            Self::Navigation => 0,
            Self::Tabs => 1,
            Self::View => 2,
            Self::PrivacySecurity => 3,
            Self::Tools => 4,
        }
    }
}

/// A single command in the palette.
#[derive(Clone)]
pub struct PaletteCommand {
    pub name: String,
    pub description: String,
    pub shortcut: String,
    /// The GAction name to activate (e.g. "win.new-tab").
    pub action: String,
    /// Symbolic icon name (e.g. "go-home-symbolic").
    pub icon: String,
    pub category: CommandCategory,
}

// ─── Recently-used tracker (in-memory, per-session) ─────────────────

thread_local! {
    static RECENT_ACTIONS: RefCell<Vec<String>> = RefCell::new(Vec::new());
}

fn record_recent(action: &str) {
    RECENT_ACTIONS.with(|recent| {
        let mut list = recent.borrow_mut();
        list.retain(|a| a != action);
        list.insert(0, action.to_string());
        list.truncate(8);
    });
}

fn get_recent() -> Vec<String> {
    RECENT_ACTIONS.with(|recent| recent.borrow().clone())
}

// ─── Build the full command list ────────────────────────────────────

pub fn build_commands() -> Vec<PaletteCommand> {
    vec![
        // ── Navigation ──────────────────────────────────────────────
        PaletteCommand {
            name: "Go Home".into(),
            description: "Navigate to the homepage".into(),
            shortcut: "Alt+Home".into(),
            action: "win.go-home".into(),
            icon: "go-home-symbolic".into(),
            category: CommandCategory::Navigation,
        },
        PaletteCommand {
            name: "Go Back".into(),
            description: "Navigate back".into(),
            shortcut: "Alt+Left".into(),
            action: "win.go-back".into(),
            icon: "go-previous-symbolic".into(),
            category: CommandCategory::Navigation,
        },
        PaletteCommand {
            name: "Go Forward".into(),
            description: "Navigate forward".into(),
            shortcut: "Alt+Right".into(),
            action: "win.go-forward".into(),
            icon: "go-next-symbolic".into(),
            category: CommandCategory::Navigation,
        },
        PaletteCommand {
            name: "Reload Page".into(),
            description: "Reload the current page".into(),
            shortcut: "F5".into(),
            action: "win.reload".into(),
            icon: "view-refresh-symbolic".into(),
            category: CommandCategory::Navigation,
        },
        PaletteCommand {
            name: "Stop Loading".into(),
            description: "Stop loading the current page".into(),
            shortcut: "".into(),
            action: "win.stop-loading".into(),
            icon: "process-stop-symbolic".into(),
            category: CommandCategory::Navigation,
        },
        PaletteCommand {
            name: "Focus Address Bar".into(),
            description: "Jump to the URL entry".into(),
            shortcut: "Ctrl+L".into(),
            action: "win.focus-url".into(),
            icon: "edit-find-symbolic".into(),
            category: CommandCategory::Navigation,
        },
        PaletteCommand {
            name: "Copy URL".into(),
            description: "Copy current page URL to clipboard".into(),
            shortcut: "Ctrl+Shift+C".into(),
            action: "win.copy-url".into(),
            icon: "edit-copy-symbolic".into(),
            category: CommandCategory::Navigation,
        },
        // ── Tabs ────────────────────────────────────────────────────
        PaletteCommand {
            name: "New Tab".into(),
            description: "Open a new browser tab".into(),
            shortcut: "Ctrl+T".into(),
            action: "win.new-tab".into(),
            icon: "tab-new-symbolic".into(),
            category: CommandCategory::Tabs,
        },
        PaletteCommand {
            name: "Close Tab".into(),
            description: "Close the current tab".into(),
            shortcut: "Ctrl+W".into(),
            action: "win.close-tab".into(),
            icon: "window-close-symbolic".into(),
            category: CommandCategory::Tabs,
        },
        PaletteCommand {
            name: "Next Tab".into(),
            description: "Switch to the next tab".into(),
            shortcut: "Ctrl+Tab".into(),
            action: "win.next-tab".into(),
            icon: "go-down-symbolic".into(),
            category: CommandCategory::Tabs,
        },
        PaletteCommand {
            name: "Previous Tab".into(),
            description: "Switch to the previous tab".into(),
            shortcut: "Ctrl+Shift+Tab".into(),
            action: "win.prev-tab".into(),
            icon: "go-up-symbolic".into(),
            category: CommandCategory::Tabs,
        },
        // ── View ────────────────────────────────────────────────────
        PaletteCommand {
            name: "Toggle Sidebar".into(),
            description: "Show or hide the vertical tab sidebar".into(),
            shortcut: "Ctrl+\\".into(),
            action: "win.toggle-sidebar".into(),
            icon: "sidebar-show-symbolic".into(),
            category: CommandCategory::View,
        },
        PaletteCommand {
            name: "Toggle Split View".into(),
            description: "Enable or disable side-by-side browsing".into(),
            shortcut: "Ctrl+Shift+E".into(),
            action: "win.toggle-split-view".into(),
            icon: "view-dual-symbolic".into(),
            category: CommandCategory::View,
        },
        PaletteCommand {
            name: "Toggle Fullscreen".into(),
            description: "Enter or exit fullscreen mode".into(),
            shortcut: "F11".into(),
            action: "win.toggle-fullscreen".into(),
            icon: "view-fullscreen-symbolic".into(),
            category: CommandCategory::View,
        },
        PaletteCommand {
            name: "Zoom In".into(),
            description: "Increase page zoom level".into(),
            shortcut: "Ctrl+=".into(),
            action: "win.zoom-in".into(),
            icon: "zoom-in-symbolic".into(),
            category: CommandCategory::View,
        },
        PaletteCommand {
            name: "Zoom Out".into(),
            description: "Decrease page zoom level".into(),
            shortcut: "Ctrl+-".into(),
            action: "win.zoom-out".into(),
            icon: "zoom-out-symbolic".into(),
            category: CommandCategory::View,
        },
        PaletteCommand {
            name: "Reset Zoom".into(),
            description: "Reset page zoom to 100%".into(),
            shortcut: "Ctrl+0".into(),
            action: "win.zoom-reset".into(),
            icon: "zoom-original-symbolic".into(),
            category: CommandCategory::View,
        },
        // ── Privacy & Security ──────────────────────────────────────
        PaletteCommand {
            name: "Toggle Ad Blocker".into(),
            description: "Enable or disable the ad blocker".into(),
            shortcut: "".into(),
            action: "win.toggle-adblock".into(),
            icon: "security-high-symbolic".into(),
            category: CommandCategory::PrivacySecurity,
        },
        PaletteCommand {
            name: "Refresh Ad Blocklist".into(),
            description: "Re-fetch Pete Lowe's ad/tracker blocklist".into(),
            shortcut: "".into(),
            action: "win.refresh-blocklist".into(),
            icon: "emblem-synchronizing-symbolic".into(),
            category: CommandCategory::PrivacySecurity,
        },
        PaletteCommand {
            name: "Clear History".into(),
            description: "Delete all browsing history".into(),
            shortcut: "".into(),
            action: "win.clear-history".into(),
            icon: "user-trash-symbolic".into(),
            category: CommandCategory::PrivacySecurity,
        },
        // ── Tools ───────────────────────────────────────────────────
        PaletteCommand {
            name: "Toggle Bookmark".into(),
            description: "Add or remove bookmark for current page".into(),
            shortcut: "Ctrl+D".into(),
            action: "win.toggle-bookmark".into(),
            icon: "starred-symbolic".into(),
            category: CommandCategory::Tools,
        },
        PaletteCommand {
            name: "Show History".into(),
            description: "Open browsing history".into(),
            shortcut: "Ctrl+H".into(),
            action: "win.show-history".into(),
            icon: "document-open-recent-symbolic".into(),
            category: CommandCategory::Tools,
        },
        PaletteCommand {
            name: "Show Bookmarks".into(),
            description: "Open bookmarks list".into(),
            shortcut: "".into(),
            action: "win.show-bookmarks".into(),
            icon: "bookmark-new-symbolic".into(),
            category: CommandCategory::Tools,
        },
        PaletteCommand {
            name: "Show Downloads".into(),
            description: "Open downloads list".into(),
            shortcut: "Ctrl+J".into(),
            action: "win.show-downloads".into(),
            icon: "folder-download-symbolic".into(),
            category: CommandCategory::Tools,
        },
        PaletteCommand {
            name: "Show Passwords".into(),
            description: "Manage saved passwords".into(),
            shortcut: "".into(),
            action: "win.show-passwords".into(),
            icon: "dialog-password-symbolic".into(),
            category: CommandCategory::Tools,
        },
        PaletteCommand {
            name: "Settings".into(),
            description: "Open browser settings".into(),
            shortcut: "Ctrl+,".into(),
            action: "win.show-settings".into(),
            icon: "emblem-system-symbolic".into(),
            category: CommandCategory::Tools,
        },
        PaletteCommand {
            name: "Print Page".into(),
            description: "Print the current page".into(),
            shortcut: "Ctrl+P".into(),
            action: "win.print-page".into(),
            icon: "printer-symbolic".into(),
            category: CommandCategory::Tools,
        },
        PaletteCommand {
            name: "View Source".into(),
            description: "View the HTML source of the current page".into(),
            shortcut: "".into(),
            action: "win.view-source".into(),
            icon: "text-x-generic-symbolic".into(),
            category: CommandCategory::Tools,
        },
        PaletteCommand {
            name: "Open Developer Tools".into(),
            description: "Open the WebKit Web Inspector".into(),
            shortcut: "F12".into(),
            action: "win.open-devtools".into(),
            icon: "utilities-terminal-symbolic".into(),
            category: CommandCategory::Tools,
        },
    ]
}

// ─── Fuzzy scoring ──────────────────────────────────────────────────

/// Score how well `query` matches `target` (case-insensitive).
/// Returns None if there is no match at all.
fn fuzzy_score(query: &str, target: &str) -> Option<i32> {
    if query.is_empty() {
        return Some(0);
    }
    let q = query.to_lowercase();
    let t = target.to_lowercase();

    if t == q {
        return Some(1000);
    }
    if t.starts_with(&q) {
        return Some(800 + q.len() as i32);
    }
    for (i, _) in t.match_indices(&q) {
        if i == 0 {
            return Some(800 + q.len() as i32);
        }
        let prev = t.as_bytes().get(i.wrapping_sub(1)).copied().unwrap_or(b' ');
        if prev == b' ' || prev == b'-' || prev == b'_' {
            return Some(600 + q.len() as i32);
        }
    }
    if t.contains(&q) {
        return Some(400 + q.len() as i32);
    }
    // Fuzzy: all chars in order.
    let mut t_iter = t.chars().enumerate().peekable();
    let mut score: i32 = 200;
    let mut prev_match_idx: Option<usize> = None;
    for qc in q.chars() {
        let mut found = false;
        while let Some(&(idx, tc)) = t_iter.peek() {
            t_iter.next();
            if tc == qc {
                if let Some(pi) = prev_match_idx {
                    if idx == pi + 1 {
                        score += 10;
                    }
                }
                prev_match_idx = Some(idx);
                found = true;
                break;
            }
        }
        if !found {
            return None;
        }
    }
    Some(score)
}

fn score_command(query: &str, cmd: &PaletteCommand) -> Option<i32> {
    let name_score = fuzzy_score(query, &cmd.name);
    let desc_score = fuzzy_score(query, &cmd.description).map(|s| s - 50);
    match (name_score, desc_score) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) => Some(a),
        (None, Some(b)) => Some(b),
        (None, None) => None,
    }
}

// ─── Palette UI ─────────────────────────────────────────────────────

/// Dismiss any existing palette overlay, if present.
fn dismiss_palette(overlay: &gtk4::Overlay) {
    // Remove any child that has the "command-palette-container" class.
    let mut child = overlay.first_child();
    while let Some(widget) = child {
        child = widget.next_sibling();
        if widget.has_css_class("command-palette-container") {
            overlay.remove_overlay(&widget);
        }
    }
}

/// Present the command palette as an in-window overlay.
pub fn show_command_palette(window: &adw::ApplicationWindow, overlay: &gtk4::Overlay) {
    // Dismiss any already-open palette first.
    dismiss_palette(overlay);

    let commands = build_commands();

    // ── Container: full-window overlay with translucent backdrop ─────
    // The container fills the whole window.  A click on the backdrop
    // (outside the palette card) dismisses it.
    let container = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    container.add_css_class("command-palette-container");
    container.set_halign(gtk4::Align::Fill);
    container.set_valign(gtk4::Align::Fill);
    container.set_hexpand(true);
    container.set_vexpand(true);

    // ── Palette card (the visible white/dark box) ───────────────────
    let card = gtk4::Box::new(gtk4::Orientation::Vertical, 0);
    card.add_css_class("command-palette");
    card.set_halign(gtk4::Align::Center);
    card.set_valign(gtk4::Align::Start);
    card.set_margin_top(80);
    card.set_width_request(520);

    // ── Search entry ────────────────────────────────────────────────
    let search_entry = gtk4::SearchEntry::new();
    search_entry.set_placeholder_text(Some("Type a command\u{2026}"));
    search_entry.set_margin_start(12);
    search_entry.set_margin_end(12);
    search_entry.set_margin_top(12);
    search_entry.set_margin_bottom(6);
    search_entry.add_css_class("command-palette-search");
    card.append(&search_entry);

    // ── Result count label ──────────────────────────────────────────
    let count_label = gtk4::Label::new(None);
    count_label.add_css_class("command-palette-count");
    count_label.add_css_class("dim-label");
    count_label.set_halign(gtk4::Align::End);
    count_label.set_margin_end(14);
    count_label.set_margin_bottom(2);
    card.append(&count_label);

    // ── Separator ───────────────────────────────────────────────────
    let sep = gtk4::Separator::new(gtk4::Orientation::Horizontal);
    sep.set_margin_start(12);
    sep.set_margin_end(12);
    card.append(&sep);

    // ── Scrollable command list ─────────────────────────────────────
    let list_box = gtk4::ListBox::new();
    list_box.set_selection_mode(gtk4::SelectionMode::Single);
    list_box.add_css_class("command-palette-list");
    list_box.set_margin_start(6);
    list_box.set_margin_end(6);
    list_box.set_margin_top(4);
    list_box.set_margin_bottom(6);

    let scrolled = gtk4::ScrolledWindow::new();
    scrolled.set_vexpand(false);
    scrolled.set_child(Some(&list_box));
    scrolled.set_min_content_height(240);
    scrolled.set_max_content_height(420);
    scrolled.set_propagate_natural_height(true);
    card.append(&scrolled);

    container.append(&card);
    overlay.add_overlay(&container);

    // ── Shared selectable-row index tracking ────────────────────────
    let selectable_indices: Rc<RefCell<Vec<i32>>> = Rc::new(RefCell::new(Vec::new()));

    // ── Populate the list ───────────────────────────────────────────
    let populate = {
        let list_box = list_box.clone();
        let commands = commands.clone();
        let overlay_ref = overlay.clone();
        let container_ref = container.clone();
        let window = window.clone();
        let count_label = count_label.clone();
        let selectable_indices = selectable_indices.clone();
        move |query: &str| {
            while let Some(child) = list_box.first_child() {
                list_box.remove(&child);
            }
            selectable_indices.borrow_mut().clear();

            let is_empty_query = query.trim().is_empty();

            if is_empty_query {
                let recent_actions = get_recent();
                let mut row_index: i32 = 0;

                if !recent_actions.is_empty() {
                    let header = make_category_header("Recently Used");
                    list_box.append(&header);
                    row_index += 1;

                    for action in &recent_actions {
                        if let Some(cmd) = commands.iter().find(|c| &c.action == action) {
                            let row = make_command_row(cmd, &overlay_ref, &container_ref, &window);
                            list_box.append(&row);
                            selectable_indices.borrow_mut().push(row_index);
                            row_index += 1;
                        }
                    }
                }

                let categories = [
                    CommandCategory::Navigation,
                    CommandCategory::Tabs,
                    CommandCategory::View,
                    CommandCategory::PrivacySecurity,
                    CommandCategory::Tools,
                ];

                for cat in &categories {
                    let cat_cmds: Vec<&PaletteCommand> =
                        commands.iter().filter(|c| c.category == *cat).collect();
                    if cat_cmds.is_empty() {
                        continue;
                    }
                    let header = make_category_header(cat.label());
                    list_box.append(&header);
                    row_index += 1;

                    for cmd in &cat_cmds {
                        let row = make_command_row(cmd, &overlay_ref, &container_ref, &window);
                        list_box.append(&row);
                        selectable_indices.borrow_mut().push(row_index);
                        row_index += 1;
                    }
                }

                count_label.set_text(&format!("{} commands", commands.len()));
            } else {
                let mut scored: Vec<(&PaletteCommand, i32)> = commands
                    .iter()
                    .filter_map(|cmd| score_command(query, cmd).map(|s| (cmd, s)))
                    .collect();
                scored.sort_by(|a, b| b.1.cmp(&a.1));

                let result_count = scored.len();

                if result_count <= 6 {
                    let mut row_index: i32 = 0;
                    for (cmd, _) in &scored {
                        let row = make_command_row(cmd, &overlay_ref, &container_ref, &window);
                        list_box.append(&row);
                        selectable_indices.borrow_mut().push(row_index);
                        row_index += 1;
                    }
                } else {
                    let mut cat_groups: std::collections::BTreeMap<
                        u8,
                        Vec<(&PaletteCommand, i32)>,
                    > = std::collections::BTreeMap::new();
                    for (cmd, score) in &scored {
                        cat_groups
                            .entry(cmd.category.sort_key())
                            .or_default()
                            .push((cmd, *score));
                    }

                    let mut cat_order: Vec<(u8, i32)> = cat_groups
                        .iter()
                        .map(|(&key, items)| (key, items.iter().map(|i| i.1).max().unwrap_or(0)))
                        .collect();
                    cat_order.sort_by(|a, b| b.1.cmp(&a.1));

                    let mut row_index: i32 = 0;
                    for (cat_key, _) in &cat_order {
                        if let Some(items) = cat_groups.get(cat_key) {
                            let cat_label = items
                                .first()
                                .map(|(c, _)| c.category.label())
                                .unwrap_or("Other");
                            let header = make_category_header(cat_label);
                            list_box.append(&header);
                            row_index += 1;

                            for (cmd, _) in items {
                                let row =
                                    make_command_row(cmd, &overlay_ref, &container_ref, &window);
                                list_box.append(&row);
                                selectable_indices.borrow_mut().push(row_index);
                                row_index += 1;
                            }
                        }
                    }
                }

                count_label.set_text(&format!("{} of {} commands", result_count, commands.len()));
            }

            let indices = selectable_indices.borrow();
            if let Some(&first_idx) = indices.first() {
                if let Some(row) = list_box.row_at_index(first_idx) {
                    list_box.select_row(Some(&row));
                }
            }
        }
    };

    populate("");

    // ── Filter on search input ──────────────────────────────────────
    let populate_for_search = populate.clone();
    search_entry.connect_search_changed(move |entry| {
        let query = entry.text().to_string();
        populate_for_search(&query);
    });

    // ── Backdrop click to dismiss ───────────────────────────────────
    let backdrop_click = gtk4::GestureClick::new();
    backdrop_click.connect_released(clone!(
        #[weak]
        overlay,
        #[weak]
        container,
        #[weak]
        card,
        move |gesture, _, x, y| {
            // Only dismiss if the click is outside the card.
            // Use compute_bounds to get card position relative to container.
            if let Some(bounds) = card.compute_bounds(&container) {
                let cx = bounds.x() as f64;
                let cy = bounds.y() as f64;
                let cw = bounds.width() as f64;
                let ch = bounds.height() as f64;
                if x < cx || x > cx + cw || y < cy || y > cy + ch {
                    overlay.remove_overlay(&container);
                    gesture.set_state(gtk4::EventSequenceState::Claimed);
                }
            } else {
                // Fallback: dismiss anyway.
                overlay.remove_overlay(&container);
                gesture.set_state(gtk4::EventSequenceState::Claimed);
            }
        }
    ));
    container.add_controller(backdrop_click);

    // ── Keyboard navigation ─────────────────────────────────────────
    // The key controller must live on the search_entry because that's
    // where focus is.  SearchEntry eats Return for its own "activate"
    // signal, so we also connect that signal for Enter handling.
    let key_controller = gtk4::EventControllerKey::new();
    key_controller.connect_key_pressed(clone!(
        #[weak]
        overlay,
        #[weak]
        list_box,
        #[weak]
        search_entry,
        #[weak]
        scrolled,
        #[strong]
        selectable_indices,
        #[upgrade_or]
        glib::Propagation::Proceed,
        move |_, key, _, _| {
            match key {
                gtk4::gdk::Key::Escape => {
                    dismiss_palette(&overlay);
                    glib::Propagation::Stop
                }
                gtk4::gdk::Key::Return | gtk4::gdk::Key::KP_Enter => {
                    if let Some(row) = list_box.selected_row() {
                        row.activate();
                    }
                    glib::Propagation::Stop
                }
                gtk4::gdk::Key::Down => {
                    move_selection(&list_box, &selectable_indices, 1, &scrolled);
                    search_entry.grab_focus();
                    glib::Propagation::Stop
                }
                gtk4::gdk::Key::Up => {
                    move_selection(&list_box, &selectable_indices, -1, &scrolled);
                    search_entry.grab_focus();
                    glib::Propagation::Stop
                }
                _ => glib::Propagation::Proceed,
            }
        }
    ));
    search_entry.add_controller(key_controller);

    // Also handle Enter via SearchEntry's own activate signal as a
    // fallback (some GTK builds swallow Return before key-pressed).
    search_entry.connect_activate(clone!(
        #[weak]
        list_box,
        move |_| {
            if let Some(row) = list_box.selected_row() {
                row.activate();
            }
        }
    ));

    // ── Focus the search entry ──────────────────────────────────────
    search_entry.grab_focus();
}

// ─── Helper: build a command row ────────────────────────────────────

fn make_command_row(
    cmd: &PaletteCommand,
    overlay: &gtk4::Overlay,
    container: &gtk4::Box,
    window: &adw::ApplicationWindow,
) -> adw::ActionRow {
    let row = adw::ActionRow::builder()
        .title(glib::markup_escape_text(&cmd.name))
        .subtitle(glib::markup_escape_text(&cmd.description))
        .activatable(true)
        .build();

    // Icon on the left.
    if !cmd.icon.is_empty() {
        let icon = gtk4::Image::from_icon_name(&cmd.icon);
        icon.set_pixel_size(18);
        icon.add_css_class("command-palette-icon");
        row.add_prefix(&icon);
    }

    // Shortcut label on the right.
    if !cmd.shortcut.is_empty() {
        let shortcut_label = gtk4::Label::new(Some(&cmd.shortcut));
        shortcut_label.add_css_class("dim-label");
        shortcut_label.add_css_class("command-palette-shortcut");
        row.add_suffix(&shortcut_label);
    }

    // Activate the action on click.
    let action_name = cmd.action.clone();
    let ov = overlay.clone();
    let ct = container.clone();
    let win = window.clone();
    row.connect_activated(move |_| {
        record_recent(&action_name);
        // Dismiss the palette first.
        ov.remove_overlay(&ct);
        // Activate the action via ActionGroup on the window.
        if let Some(name) = action_name.strip_prefix("win.") {
            gtk4::gio::prelude::ActionGroupExt::activate_action(&win, name, None);
        }
    });

    row
}

// ─── Helper: build a non-selectable category header row ─────────────

fn make_category_header(label: &str) -> gtk4::ListBoxRow {
    let row = gtk4::ListBoxRow::new();
    row.set_selectable(false);
    row.set_activatable(false);
    row.add_css_class("command-palette-category-header");

    let lbl = gtk4::Label::new(Some(label));
    lbl.set_halign(gtk4::Align::Start);
    lbl.set_margin_start(8);
    lbl.set_margin_top(6);
    lbl.set_margin_bottom(2);
    lbl.add_css_class("caption-heading");
    lbl.add_css_class("dim-label");
    row.set_child(Some(&lbl));

    row
}

// ─── Helper: move selection by delta among selectable rows ──────────

fn move_selection(
    list_box: &gtk4::ListBox,
    selectable_indices: &Rc<RefCell<Vec<i32>>>,
    delta: i32,
    scrolled: &gtk4::ScrolledWindow,
) {
    let indices = selectable_indices.borrow();
    if indices.is_empty() {
        return;
    }

    let current_idx = list_box.selected_row().map(|r| r.index()).unwrap_or(-1);

    let current_pos = indices.iter().position(|&i| i == current_idx);

    let new_pos = match current_pos {
        Some(pos) => {
            let new = pos as i32 + delta;
            new.clamp(0, indices.len() as i32 - 1) as usize
        }
        None => {
            if delta > 0 {
                0
            } else {
                indices.len() - 1
            }
        }
    };

    if let Some(&row_idx) = indices.get(new_pos) {
        if let Some(row) = list_box.row_at_index(row_idx) {
            list_box.select_row(Some(&row));
            // Scroll the selected row into view.
            let adj = scrolled.vadjustment();
            if let Some(bounds) = row.compute_bounds(list_box) {
                let row_y = bounds.y() as f64;
                let row_h = bounds.height() as f64;
                let visible_top = adj.value();
                let visible_h = adj.page_size();
                if row_y < visible_top {
                    adj.set_value(row_y);
                } else if row_y + row_h > visible_top + visible_h {
                    adj.set_value(row_y + row_h - visible_h);
                }
            }
        }
    }
}
