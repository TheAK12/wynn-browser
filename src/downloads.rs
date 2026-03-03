// ─────────────────────────────────────────────────────────────────────
// downloads.rs  –  Download manager for Wynn Browser
// ─────────────────────────────────────────────────────────────────────
//
// Hooks into WebKit's NetworkSession download signals to track active
// and completed downloads.  Provides a dialog to view download
// progress and open completed files.
//
// Public API:
//
//   setup_download_handler(network_session) – connect download signals.
//   show_downloads_dialog(window)           – present the downloads UI.
// ─────────────────────────────────────────────────────────────────────

use glib::clone;
use gtk4::prelude::*;
use libadwaita as adw;
use libadwaita::prelude::*;

use std::cell::RefCell;
use std::rc::Rc;

/// Represents a single download for the UI.
#[derive(Debug, Clone)]
pub struct DownloadInfo {
    pub filename: String,
    pub destination: String,
    pub progress: f64,
    pub finished: bool,
    pub failed: bool,
    pub error_msg: String,
}

/// Shared download list (GTK is single-threaded, so Rc<RefCell> is fine).
pub type DownloadList = Rc<RefCell<Vec<DownloadInfo>>>;

/// Create a new shared download list.
pub fn new_download_list() -> DownloadList {
    Rc::new(RefCell::new(Vec::new()))
}

/// Connect download signals on the given [`webkit6::NetworkSession`].
///
/// When a download starts, we set its destination to `~/Downloads/`
/// and track its progress in the shared `DownloadList`.
pub fn setup_download_handler(session: &webkit6::NetworkSession, downloads: &DownloadList) {
    session.connect_download_started(clone!(
        #[strong]
        downloads,
        move |_session, download| {
            let dl_list = downloads.clone();

            // ── Decide destination ──────────────────────────────────
            download.connect_decide_destination(clone!(
                #[strong]
                dl_list,
                move |dl, suggested_filename| {
                    let download_dir = download_directory();
                    std::fs::create_dir_all(&download_dir).ok();

                    let dest_path = download_dir.join(suggested_filename);
                    let dest_uri = format!("file://{}", dest_path.display());
                    dl.set_destination(&dest_uri);

                    // Add to the tracking list.
                    dl_list.borrow_mut().push(DownloadInfo {
                        filename: suggested_filename.to_string(),
                        destination: dest_uri,
                        progress: 0.0,
                        finished: false,
                        failed: false,
                        error_msg: String::new(),
                    });

                    true // we handled the destination
                }
            ));

            // ── Progress updates ────────────────────────────────────
            download.connect_estimated_progress_notify(clone!(
                #[strong]
                dl_list,
                move |dl| {
                    let progress = dl.estimated_progress();
                    if let Some(dest) = dl.destination() {
                        let mut list = dl_list.borrow_mut();
                        if let Some(info) = list.iter_mut().find(|i| i.destination == dest.as_str())
                        {
                            info.progress = progress;
                        }
                    }
                }
            ));

            // ── Finished ────────────────────────────────────────────
            download.connect_finished(clone!(
                #[strong]
                dl_list,
                move |dl| {
                    if let Some(dest) = dl.destination() {
                        let mut list = dl_list.borrow_mut();
                        if let Some(info) = list.iter_mut().find(|i| i.destination == dest.as_str())
                        {
                            info.progress = 1.0;
                            info.finished = true;
                        }
                    }
                }
            ));

            // ── Failed ──────────────────────────────────────────────
            download.connect_failed(clone!(
                #[strong]
                dl_list,
                move |dl, error| {
                    if let Some(dest) = dl.destination() {
                        let mut list = dl_list.borrow_mut();
                        if let Some(info) = list.iter_mut().find(|i| i.destination == dest.as_str())
                        {
                            info.failed = true;
                            info.error_msg = error.message().to_string();
                        }
                    }
                }
            ));
        }
    ));
}

/// Present a dialog showing download history and progress.
pub fn show_downloads_dialog(window: &adw::ApplicationWindow, downloads: &DownloadList) {
    let dialog = gtk4::Window::builder()
        .title("Downloads")
        .transient_for(window)
        .modal(true)
        .default_width(500)
        .default_height(400)
        .build();

    let vbox = gtk4::Box::new(gtk4::Orientation::Vertical, 0);

    let header = adw::HeaderBar::new();
    vbox.append(&header);

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
    let entries = downloads.borrow().clone();

    if entries.is_empty() {
        let label = gtk4::Label::new(Some("No downloads"));
        label.add_css_class("dim-label");
        label.set_margin_top(24);
        label.set_margin_bottom(24);
        list_box.append(&label);
    } else {
        for dl in entries.iter().rev() {
            let subtitle = if dl.failed {
                format!("Failed: {}", dl.error_msg)
            } else if dl.finished {
                "Completed".to_string()
            } else {
                format!("{:.0}%", dl.progress * 100.0)
            };

            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&dl.filename))
                .subtitle(glib::markup_escape_text(&subtitle))
                .build();

            // Status icon.
            let icon_name = if dl.failed {
                "dialog-error-symbolic"
            } else if dl.finished {
                "emblem-ok-symbolic"
            } else {
                "content-loading-symbolic"
            };
            let icon = gtk4::Image::from_icon_name(icon_name);
            row.add_prefix(&icon);

            // Show a progress bar for in-progress downloads.
            if !dl.finished && !dl.failed {
                let progress = gtk4::ProgressBar::new();
                progress.set_fraction(dl.progress);
                progress.set_valign(gtk4::Align::Center);
                progress.set_hexpand(false);
                progress.set_size_request(100, -1);
                row.add_suffix(&progress);
            }

            list_box.append(&row);
        }
    }

    dialog.present();
}

/// Return the default download directory.
fn download_directory() -> std::path::PathBuf {
    std::env::var("XDG_DOWNLOAD_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| {
            let home = std::env::var("HOME").unwrap_or_else(|_| "/tmp".to_string());
            std::path::PathBuf::from(home).join("Downloads")
        })
}
