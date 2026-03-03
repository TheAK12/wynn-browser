// ---------------------------------------------------------------------
// passwords.rs  --  Password manager for Wynn Browser
// ---------------------------------------------------------------------
//
// Provides encrypted credential storage in SQLite, autofill via JS
// injection, and a management UI dialog.
//
// Encryption:
//   - Master password is required to unlock the password store.
//   - Argon2id KDF derives a 256-bit key from the master password and
//     a per-install random salt (stored in the `settings` table).
//   - AES-256-GCM authenticated encryption protects each password.
//   - Each password gets its own random 96-bit nonce.
//   - DB column `password_enc` stores: base64(nonce || ciphertext || tag)
//   - A verification token (encrypted known string) lets us check the
//     master password without storing it.
//   - The derived key is cached in memory for the session; never
//     written to disk.
//
// Public API:
//
//   save_credential(origin, username, password)
//   lookup_credentials(origin) -> Vec<SavedCredential>
//   all_credentials() -> Vec<SavedCredential>
//   delete_credential(id)
//   update_credential(id, username, password)
//   record_use(id)
//   autofill_js(origin) -> Option<String>
//   show_passwords_dialog(window)
//   show_save_password_prompt(window, origin, username, password)
//   is_never_save(origin) -> bool
//   extract_origin(url) -> String
//   ensure_unlocked(widget, callback)  -- prompt for master password
// ---------------------------------------------------------------------

use adw::prelude::*;
use gtk4::prelude::*;
use libadwaita as adw;

use aes_gcm::aead::{Aead, KeyInit, OsRng};
use aes_gcm::{Aes256Gcm, Nonce};
use argon2::Argon2;
use rand::RngCore;

use std::cell::RefCell;

use crate::database;

// ── Session key cache ────────────────────────────────────────────────
//
// The derived 256-bit key is cached in a thread-local RefCell for the
// lifetime of the GTK main loop (single-threaded).  It is never
// persisted to disk.

thread_local! {
    static DERIVED_KEY: RefCell<Option<[u8; 32]>> = const { RefCell::new(None) };
}

/// Check whether the master password has been entered this session.
fn is_unlocked() -> bool {
    DERIVED_KEY.with(|k| k.borrow().is_some())
}

/// Cache the derived key for this session.
fn set_derived_key(key: [u8; 32]) {
    DERIVED_KEY.with(|k| *k.borrow_mut() = Some(key));
}

/// Retrieve the cached derived key.  Returns `None` if locked.
fn get_derived_key() -> Option<[u8; 32]> {
    DERIVED_KEY.with(|k| *k.borrow())
}

// ── Argon2 key derivation ────────────────────────────────────────────

/// Argon2id parameters — tuned for interactive use (not too slow, but
/// resistant to brute-force).
const ARGON2_SALT_LEN: usize = 16;
const ARGON2_KEY_LEN: usize = 32;

/// Derive a 256-bit key from the master password and salt using Argon2id.
fn derive_key(master_password: &str, salt: &[u8]) -> [u8; ARGON2_KEY_LEN] {
    let mut key = [0u8; ARGON2_KEY_LEN];
    // Default Argon2id params: m=19456 KiB, t=2, p=1
    Argon2::default()
        .hash_password_into(master_password.as_bytes(), salt, &mut key)
        .expect("Argon2 key derivation failed");
    key
}

/// Generate a random salt for Argon2.
fn generate_salt() -> [u8; ARGON2_SALT_LEN] {
    let mut salt = [0u8; ARGON2_SALT_LEN];
    OsRng.fill_bytes(&mut salt);
    salt
}

// ── AES-256-GCM encryption / decryption ─────────────────────────────

const AES_NONCE_LEN: usize = 12;

/// Encrypt `plaintext` with AES-256-GCM using the given key.
/// Returns `nonce || ciphertext_with_tag` (12 + len + 16 bytes).
fn aes_encrypt(plaintext: &[u8], key: &[u8; 32]) -> Vec<u8> {
    let cipher = Aes256Gcm::new(key.into());
    let mut nonce_bytes = [0u8; AES_NONCE_LEN];
    OsRng.fill_bytes(&mut nonce_bytes);
    let nonce = Nonce::from_slice(&nonce_bytes);
    let ciphertext = cipher
        .encrypt(nonce, plaintext)
        .expect("AES-256-GCM encryption failed");
    let mut result = Vec::with_capacity(AES_NONCE_LEN + ciphertext.len());
    result.extend_from_slice(&nonce_bytes);
    result.extend_from_slice(&ciphertext);
    result
}

/// Decrypt `data` (nonce || ciphertext_with_tag) with AES-256-GCM.
/// Returns `None` if decryption or authentication fails (wrong key).
fn aes_decrypt(data: &[u8], key: &[u8; 32]) -> Option<Vec<u8>> {
    if data.len() < AES_NONCE_LEN + 16 {
        return None; // Too short — need at least nonce + tag.
    }
    let cipher = Aes256Gcm::new(key.into());
    let nonce = Nonce::from_slice(&data[..AES_NONCE_LEN]);
    cipher.decrypt(nonce, &data[AES_NONCE_LEN..]).ok()
}

// ── Base64 helpers (minimal, no extra crate) ─────────────────────────

fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::new();
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;
        result.push(CHARS[((triple >> 18) & 0x3f) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3f) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3f) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3f) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

fn base64_decode(input: &str) -> Option<Vec<u8>> {
    fn val(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'+' => Some(62),
            b'/' => Some(63),
            b'=' => Some(0),
            _ => None,
        }
    }
    let bytes: Vec<u8> = input.bytes().filter(|b| !b.is_ascii_whitespace()).collect();
    if bytes.len() % 4 != 0 {
        return None;
    }
    let mut result = Vec::new();
    for chunk in bytes.chunks(4) {
        let a = val(chunk[0])?;
        let b = val(chunk[1])?;
        let c = val(chunk[2])?;
        let d = val(chunk[3])?;
        let triple = (a << 18) | (b << 12) | (c << 6) | d;
        result.push(((triple >> 16) & 0xff) as u8);
        if chunk[2] != b'=' {
            result.push(((triple >> 8) & 0xff) as u8);
        }
        if chunk[3] != b'=' {
            result.push((triple & 0xff) as u8);
        }
    }
    Some(result)
}

// ── High-level encrypt / decrypt using session key ──────────────────

/// Encrypt a password string.  Panics if the session is locked.
fn encrypt_password(plaintext: &str) -> String {
    let key = get_derived_key().expect("Password store is locked");
    let encrypted = aes_encrypt(plaintext.as_bytes(), &key);
    base64_encode(&encrypted)
}

/// Decrypt a base64-encoded ciphertext.  Returns empty string on
/// failure (wrong key or corrupt data).
fn decrypt_password(ciphertext: &str) -> String {
    let key = match get_derived_key() {
        Some(k) => k,
        None => return String::new(),
    };
    if let Some(data) = base64_decode(ciphertext) {
        if let Some(plaintext) = aes_decrypt(&data, &key) {
            return String::from_utf8(plaintext).unwrap_or_default();
        }
    }
    String::new()
}

// ── Master password setup & verification ─────────────────────────────
//
// Settings keys:
//   `pw_salt`   — base64-encoded 16-byte Argon2 salt
//   `pw_verify` — AES-256-GCM encrypted known string ("wynn-pw-ok")
//
// On first use: user sets a master password → we generate salt, derive
// key, encrypt the verification token, store salt + token in settings.
//
// On subsequent launches: user enters master password → we load salt,
// derive key, try to decrypt the verification token.  If it decrypts
// to "wynn-pw-ok" the password is correct.

const VERIFY_PLAINTEXT: &str = "wynn-pw-ok";

/// Check whether a master password has been configured.
pub fn has_master_password() -> bool {
    let salt = crate::settings::get_setting("pw_salt").unwrap_or_default();
    let verify = crate::settings::get_setting("pw_verify").unwrap_or_default();
    !salt.is_empty() && !verify.is_empty()
}

/// Attempt to unlock with a master password.  Returns `true` on success.
fn try_unlock(master_password: &str) -> bool {
    let salt_b64 = crate::settings::get_setting("pw_salt").unwrap_or_default();
    let verify_b64 = crate::settings::get_setting("pw_verify").unwrap_or_default();

    if salt_b64.is_empty() || verify_b64.is_empty() {
        return false;
    }

    let salt = match base64_decode(&salt_b64) {
        Some(s) if s.len() >= ARGON2_SALT_LEN => s,
        _ => return false,
    };

    let key = derive_key(master_password, &salt[..ARGON2_SALT_LEN]);

    // Try to decrypt the verification token.
    if let Some(data) = base64_decode(&verify_b64) {
        if let Some(plaintext) = aes_decrypt(&data, &key) {
            if plaintext == VERIFY_PLAINTEXT.as_bytes() {
                set_derived_key(key);
                return true;
            }
        }
    }
    false
}

/// Set up a new master password.  Generates salt, derives key,
/// encrypts verification token, and stores everything.
fn setup_master_password(master_password: &str) {
    let salt = generate_salt();
    let key = derive_key(master_password, &salt);

    // Encrypt the verification token.
    let verify_encrypted = aes_encrypt(VERIFY_PLAINTEXT.as_bytes(), &key);

    crate::settings::set_setting("pw_salt", &base64_encode(&salt));
    crate::settings::set_setting("pw_verify", &base64_encode(&verify_encrypted));

    set_derived_key(key);
}

// ── Master password UI ──────────────────────────────────────────────

/// Prompt the user for their master password (or to set one), then
/// call `on_unlocked` once the store is unlocked.
///
/// If already unlocked this session, calls `on_unlocked` immediately.
pub fn ensure_unlocked<F: Fn() + 'static>(widget: &impl IsA<gtk4::Widget>, on_unlocked: F) {
    if is_unlocked() {
        on_unlocked();
        return;
    }

    let on_unlocked: std::rc::Rc<dyn Fn()> = std::rc::Rc::new(on_unlocked);

    if has_master_password() {
        show_unlock_dialog(widget, on_unlocked);
    } else {
        show_setup_dialog(widget, on_unlocked);
    }
}

/// Dialog to enter an existing master password.
fn show_unlock_dialog(widget: &impl IsA<gtk4::Widget>, on_unlocked: std::rc::Rc<dyn Fn()>) {
    let dialog = adw::AlertDialog::builder()
        .heading("Unlock Password Store")
        .body("Enter your master password to access saved passwords.")
        .close_response("cancel")
        .default_response("unlock")
        .build();

    dialog.add_response("cancel", "Cancel");
    dialog.add_response("unlock", "Unlock");
    dialog.set_response_appearance("unlock", adw::ResponseAppearance::Suggested);

    let form_box = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    form_box.set_margin_top(4);

    let pw_entry = gtk4::PasswordEntry::new();
    pw_entry.set_placeholder_text(Some("Master Password"));
    pw_entry.set_show_peek_icon(true);
    form_box.append(&pw_entry);

    let error_label = gtk4::Label::new(None);
    error_label.add_css_class("error");
    error_label.set_visible(false);
    form_box.append(&error_label);

    dialog.set_extra_child(Some(&form_box));

    let widget_clone = widget.upcast_ref::<gtk4::Widget>().clone();

    dialog.connect_response(None, move |_dlg, response| {
        if response == "unlock" {
            let master = pw_entry.text().to_string();
            if master.is_empty() {
                return;
            }
            if try_unlock(&master) {
                (on_unlocked)();
            } else {
                // Wrong password — show error and re-prompt.
                error_label.set_label("Incorrect master password.");
                error_label.set_visible(true);
                // Re-show the dialog after a short delay.
                let w = widget_clone.clone();
                let cb = on_unlocked.clone();
                glib::timeout_add_local_once(std::time::Duration::from_millis(300), move || {
                    show_unlock_dialog(&w, cb);
                });
            }
        }
    });

    dialog.present(Some(widget));
}

/// Dialog to set a new master password (first-time setup).
fn show_setup_dialog(widget: &impl IsA<gtk4::Widget>, on_unlocked: std::rc::Rc<dyn Fn()>) {
    let dialog = adw::AlertDialog::builder()
        .heading("Set Master Password")
        .body(
            "Choose a master password to protect your saved passwords.\n\
             This password is never stored and cannot be recovered.",
        )
        .close_response("cancel")
        .default_response("set")
        .build();

    dialog.add_response("cancel", "Cancel");
    dialog.add_response("set", "Set Password");
    dialog.set_response_appearance("set", adw::ResponseAppearance::Suggested);

    let form_box = gtk4::Box::new(gtk4::Orientation::Vertical, 8);
    form_box.set_margin_top(4);

    let pw_entry = gtk4::PasswordEntry::new();
    pw_entry.set_placeholder_text(Some("Master Password"));
    pw_entry.set_show_peek_icon(true);
    form_box.append(&pw_entry);

    let confirm_entry = gtk4::PasswordEntry::new();
    confirm_entry.set_placeholder_text(Some("Confirm Password"));
    confirm_entry.set_show_peek_icon(true);
    form_box.append(&confirm_entry);

    let error_label = gtk4::Label::new(None);
    error_label.add_css_class("error");
    error_label.set_visible(false);
    form_box.append(&error_label);

    dialog.set_extra_child(Some(&form_box));

    let widget_clone = widget.upcast_ref::<gtk4::Widget>().clone();

    dialog.connect_response(None, move |_dlg, response| {
        if response == "set" {
            let master = pw_entry.text().to_string();
            let confirm = confirm_entry.text().to_string();

            if master.is_empty() {
                error_label.set_label("Password cannot be empty.");
                error_label.set_visible(true);
                // Re-show.
                let _w = widget_clone.clone();
                let cb_text = "re-show";
                let _ = cb_text;
                return;
            }
            if master.len() < 6 {
                error_label.set_label("Password must be at least 6 characters.");
                error_label.set_visible(true);
                return;
            }
            if master != confirm {
                error_label.set_label("Passwords do not match.");
                error_label.set_visible(true);
                return;
            }

            setup_master_password(&master);
            on_unlocked();
        }
    });

    dialog.present(Some(widget));
}

// ── Data model ───────────────────────────────────────────────────────

/// A saved login credential.
#[derive(Clone, Debug)]
pub struct SavedCredential {
    pub id: i64,
    pub origin: String,
    pub username: String,
    pub password: String, // Decrypted plaintext.
    pub created: String,
    pub last_used: String,
    pub use_count: i64,
}

// ── CRUD operations ──────────────────────────────────────────────────

/// Extract the origin (scheme + host) from a URL.
pub fn extract_origin(url: &str) -> String {
    if let Some(idx) = url.find("://") {
        let after = &url[idx + 3..];
        if let Some(end) = after.find('/') {
            url[..idx + 3 + end].to_string()
        } else {
            url.to_string()
        }
    } else {
        url.to_string()
    }
}

/// Save a credential.  If one already exists for (origin, username),
/// update the password instead.
///
/// Requires the store to be unlocked (panics otherwise).
pub fn save_credential(origin: &str, username: &str, password: &str) {
    if !is_unlocked() {
        return;
    }
    let enc = encrypt_password(password);
    database::with_db(|conn| {
        conn.execute(
            "INSERT INTO passwords (origin, username, password_enc)
             VALUES (?1, ?2, ?3)
             ON CONFLICT(origin, username)
             DO UPDATE SET password_enc = ?3,
                           last_used = datetime('now'),
                           use_count = use_count + 1",
            rusqlite::params![origin, username, enc],
        )
        .expect("Failed to save credential");
    });
}

/// Look up all saved credentials for a given origin.
///
/// Returns empty vec if the store is locked.
pub fn lookup_credentials(origin: &str) -> Vec<SavedCredential> {
    if !is_unlocked() {
        return Vec::new();
    }
    database::with_db(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT id, origin, username, password_enc, created, last_used, use_count
                 FROM passwords WHERE origin = ?1
                 ORDER BY use_count DESC, last_used DESC",
            )
            .expect("Failed to prepare credential lookup");

        stmt.query_map(rusqlite::params![origin], |row| {
            let enc: String = row.get(3)?;
            Ok(SavedCredential {
                id: row.get(0)?,
                origin: row.get(1)?,
                username: row.get(2)?,
                password: decrypt_password(&enc),
                created: row.get(4)?,
                last_used: row.get(5)?,
                use_count: row.get(6)?,
            })
        })
        .expect("Failed to query credentials")
        .filter_map(|r| r.ok())
        .collect()
    })
}

/// Return all saved credentials (for the management UI).
///
/// Returns empty vec if the store is locked.
pub fn all_credentials() -> Vec<SavedCredential> {
    if !is_unlocked() {
        return Vec::new();
    }
    database::with_db(|conn| {
        let mut stmt = conn
            .prepare(
                "SELECT id, origin, username, password_enc, created, last_used, use_count
                 FROM passwords ORDER BY origin, username",
            )
            .expect("Failed to prepare all credentials query");

        stmt.query_map([], |row| {
            let enc: String = row.get(3)?;
            Ok(SavedCredential {
                id: row.get(0)?,
                origin: row.get(1)?,
                username: row.get(2)?,
                password: decrypt_password(&enc),
                created: row.get(4)?,
                last_used: row.get(5)?,
                use_count: row.get(6)?,
            })
        })
        .expect("Failed to query all credentials")
        .filter_map(|r| r.ok())
        .collect()
    })
}

/// Delete a credential by ID.
pub fn delete_credential(id: i64) {
    database::with_db(|conn| {
        conn.execute("DELETE FROM passwords WHERE id = ?1", rusqlite::params![id])
            .expect("Failed to delete credential");
    });
}

/// Update a credential's username and password.
///
/// Requires the store to be unlocked.
pub fn update_credential(id: i64, username: &str, password: &str) {
    if !is_unlocked() {
        return;
    }
    let enc = encrypt_password(password);
    database::with_db(|conn| {
        conn.execute(
            "UPDATE passwords SET username = ?2, password_enc = ?3 WHERE id = ?1",
            rusqlite::params![id, username, enc],
        )
        .expect("Failed to update credential");
    });
}

/// Record a credential use (bump use_count and last_used).
pub fn record_use(id: i64) {
    database::with_db(|conn| {
        conn.execute(
            "UPDATE passwords SET use_count = use_count + 1,
                                  last_used = datetime('now')
             WHERE id = ?1",
            rusqlite::params![id],
        )
        .expect("Failed to record credential use");
    });
}

// ── Autofill JavaScript ──────────────────────────────────────────────

/// Generate JavaScript that fills saved credentials into login forms.
///
/// Returns `None` if the store is locked or no credentials exist for
/// this origin.
pub fn autofill_js(origin: &str) -> Option<String> {
    let creds = lookup_credentials(origin);
    if creds.is_empty() {
        return None;
    }

    // Use the first (most used) credential.
    let cred = &creds[0];
    let username_escaped = cred.username.replace('\\', "\\\\").replace('\'', "\\'");
    let password_escaped = cred.password.replace('\\', "\\\\").replace('\'', "\\'");

    Some(format!(
        r#"(function() {{
    'use strict';
    function fill() {{
        var pwFields = document.querySelectorAll('input[type="password"]');
        if (pwFields.length === 0) return false;
        pwFields.forEach(function(pw) {{
            var form = pw.closest('form') || document.body;
            var inputs = form.querySelectorAll(
                'input[type="text"], input[type="email"], input[type="tel"], input:not([type])'
            );
            var userField = null;
            for (var i = 0; i < inputs.length; i++) {{
                if (inputs[i].compareDocumentPosition(pw) & 4) {{
                    userField = inputs[i];
                }}
            }}
            if (userField) {{
                var nativeSet = Object.getOwnPropertyDescriptor(
                    HTMLInputElement.prototype, 'value'
                ).set;
                nativeSet.call(userField, '{username}');
                userField.dispatchEvent(new Event('input', {{bubbles: true}}));
                userField.dispatchEvent(new Event('change', {{bubbles: true}}));
            }}
            var nativeSet = Object.getOwnPropertyDescriptor(
                HTMLInputElement.prototype, 'value'
            ).set;
            nativeSet.call(pw, '{password}');
            pw.dispatchEvent(new Event('input', {{bubbles: true}}));
            pw.dispatchEvent(new Event('change', {{bubbles: true}}));
        }});
        return true;
    }}
    if (!fill()) {{
        setTimeout(fill, 800);
        setTimeout(fill, 2000);
    }}
}})();"#,
        username = username_escaped,
        password = password_escaped,
    ))
}

// ── UI: Save password prompt ─────────────────────────────────────────

/// Show a "Save password?" dialog after detecting a form submission
/// with credentials.
///
/// If the store is locked, prompts for the master password first.
pub fn show_save_password_prompt(
    window: &impl IsA<gtk4::Widget>,
    origin: &str,
    username: &str,
    password: &str,
) {
    let origin_owned = origin.to_string();
    let username_owned = username.to_string();
    let password_owned = password.to_string();
    let widget = window.upcast_ref::<gtk4::Widget>().clone();
    let widget_for_closure = widget.clone();

    ensure_unlocked(&widget, move || {
        do_save_prompt(
            &widget_for_closure,
            &origin_owned,
            &username_owned,
            &password_owned,
        );
    });
}

/// Internal: actually show the save prompt (store is already unlocked).
fn do_save_prompt(widget: &gtk4::Widget, origin: &str, username: &str, password: &str) {
    // Check if we already have this exact credential saved.
    let existing = lookup_credentials(origin);
    for cred in &existing {
        if cred.username == username && cred.password == password {
            // Already saved with same password — just bump usage.
            record_use(cred.id);
            return;
        }
    }

    // Check if we have the same username but different password.
    let is_update = existing.iter().any(|c| c.username == username);
    let heading = if is_update {
        "Update password?"
    } else {
        "Save password?"
    };
    let body = format!(
        "Would you like to {} your password for {}?\n\nUsername: {}",
        if is_update { "update" } else { "save" },
        origin,
        if username.is_empty() {
            "(none)"
        } else {
            username
        },
    );

    let dialog = adw::AlertDialog::builder()
        .heading(heading)
        .body(&body)
        .close_response("never")
        .default_response("save")
        .build();

    dialog.add_response("never", "Never");
    dialog.add_response("not-now", "Not Now");
    dialog.add_response("save", if is_update { "Update" } else { "Save" });
    dialog.set_response_appearance("save", adw::ResponseAppearance::Suggested);

    let icon = gtk4::Image::from_icon_name("dialog-password-symbolic");
    icon.set_pixel_size(48);
    icon.set_margin_bottom(8);
    dialog.set_extra_child(Some(&icon));

    let origin_owned = origin.to_string();
    let username_owned = username.to_string();
    let password_owned = password.to_string();

    dialog.connect_response(None, move |_dlg, response| match response {
        "save" => {
            save_credential(&origin_owned, &username_owned, &password_owned);
        }
        "never" => {
            // Save a "never save" marker.
            save_credential(&origin_owned, "\x00never\x00", "");
        }
        _ => {
            // "Not Now" — do nothing, ask again next time.
        }
    });

    dialog.present(Some(widget));
}

/// Check whether the user has opted out of saving passwords for this
/// origin (clicked "Never").
pub fn is_never_save(origin: &str) -> bool {
    // This works even when locked because we check the username field
    // (which is stored in plaintext in the DB).
    database::with_db(|conn| {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM passwords WHERE origin = ?1 AND username = ?2",
                rusqlite::params![origin, "\x00never\x00"],
                |row| row.get(0),
            )
            .unwrap_or(0);
        count > 0
    })
}

// ── UI: Password management dialog ───────────────────────────────────

/// Show the password management dialog.
///
/// Prompts for master password if locked.
pub fn show_passwords_dialog(window: &impl IsA<gtk4::Widget>) {
    let widget = window.upcast_ref::<gtk4::Widget>().clone();
    let widget_for_closure = widget.clone();

    ensure_unlocked(&widget, move || {
        do_show_passwords_dialog(&widget_for_closure);
    });
}

/// Internal: build and present the passwords dialog (already unlocked).
fn do_show_passwords_dialog(widget: &gtk4::Widget) {
    let dialog = adw::Dialog::builder()
        .title("Saved Passwords")
        .content_width(550)
        .content_height(500)
        .build();

    let toolbar_view = adw::ToolbarView::new();
    let header = adw::HeaderBar::new();
    toolbar_view.add_top_bar(&header);

    let content_box = gtk4::Box::new(gtk4::Orientation::Vertical, 0);

    // Search bar at the top.
    let search_entry = gtk4::SearchEntry::new();
    search_entry.set_placeholder_text(Some("Search passwords\u{2026}"));
    search_entry.set_margin_start(12);
    search_entry.set_margin_end(12);
    search_entry.set_margin_top(8);
    search_entry.set_margin_bottom(8);
    content_box.append(&search_entry);

    let sep = gtk4::Separator::new(gtk4::Orientation::Horizontal);
    content_box.append(&sep);

    // Scrollable list of passwords.
    let scrolled = gtk4::ScrolledWindow::new();
    scrolled.set_vexpand(true);
    scrolled.set_hscrollbar_policy(gtk4::PolicyType::Never);

    let list_box = gtk4::ListBox::new();
    list_box.set_selection_mode(gtk4::SelectionMode::None);
    list_box.add_css_class("boxed-list");
    list_box.set_margin_start(12);
    list_box.set_margin_end(12);
    list_box.set_margin_top(8);
    list_box.set_margin_bottom(8);

    let creds = all_credentials();
    let no_passwords = creds
        .iter()
        .filter(|c| c.username != "\x00never\x00")
        .count()
        == 0;

    if no_passwords {
        let placeholder = adw::StatusPage::builder()
            .icon_name("dialog-password-symbolic")
            .title("No Saved Passwords")
            .description("Passwords you save will appear here.")
            .build();
        scrolled.set_child(Some(&placeholder));
    } else {
        for cred in &creds {
            // Skip "never save" markers.
            if cred.username == "\x00never\x00" {
                continue;
            }

            let row = adw::ActionRow::builder()
                .title(&cred.origin)
                .subtitle(&cred.username)
                .build();
            row.add_prefix(&gtk4::Image::from_icon_name("dialog-password-symbolic"));

            // Copy password button.
            let copy_btn = gtk4::Button::from_icon_name("edit-copy-symbolic");
            copy_btn.set_tooltip_text(Some("Copy password"));
            copy_btn.add_css_class("flat");
            copy_btn.set_valign(gtk4::Align::Center);
            let pw_copy = cred.password.clone();
            copy_btn.connect_clicked(move |btn| {
                if let Some(display) = gtk4::gdk::Display::default() {
                    display.clipboard().set_text(&pw_copy);
                    btn.set_icon_name("emblem-ok-symbolic");
                    let btn_weak = btn.downgrade();
                    glib::timeout_add_local_once(std::time::Duration::from_secs(2), move || {
                        if let Some(b) = btn_weak.upgrade() {
                            b.set_icon_name("edit-copy-symbolic");
                        }
                    });
                }
            });
            row.add_suffix(&copy_btn);

            // Toggle password visibility button.
            let reveal_btn = gtk4::Button::from_icon_name("view-reveal-symbolic");
            reveal_btn.set_tooltip_text(Some("Show password"));
            reveal_btn.add_css_class("flat");
            reveal_btn.set_valign(gtk4::Align::Center);
            let pw_reveal = cred.password.clone();
            let user_reveal = cred.username.clone();
            let origin_reveal = cred.origin.clone();
            let row_weak = row.downgrade();
            reveal_btn.connect_clicked(move |btn| {
                let is_showing = btn
                    .icon_name()
                    .map(|n| n == "view-conceal-symbolic")
                    .unwrap_or(false);
                if let Some(r) = row_weak.upgrade() {
                    if is_showing {
                        r.set_subtitle(&user_reveal);
                        btn.set_icon_name("view-reveal-symbolic");
                        btn.set_tooltip_text(Some("Show password"));
                    } else {
                        let masked: String = "\u{2022}".repeat(pw_reveal.len().min(20));
                        r.set_subtitle(&format!("{}\n{}", user_reveal, masked));
                        btn.set_icon_name("view-conceal-symbolic");
                        btn.set_tooltip_text(Some("Hide password"));
                    }
                }
                let _ = &origin_reveal; // keep alive
            });
            row.add_suffix(&reveal_btn);

            // Delete button.
            let delete_btn = gtk4::Button::from_icon_name("user-trash-symbolic");
            delete_btn.set_tooltip_text(Some("Delete"));
            delete_btn.add_css_class("flat");
            delete_btn.set_valign(gtk4::Align::Center);
            let cred_id = cred.id;
            let list_weak = list_box.downgrade();
            let row_ref = row.clone();
            delete_btn.connect_clicked(move |_| {
                delete_credential(cred_id);
                if let Some(lb) = list_weak.upgrade() {
                    lb.remove(&row_ref);
                }
            });
            row.add_suffix(&delete_btn);

            list_box.append(&row);
        }
        scrolled.set_child(Some(&list_box));
    }

    // Search filter.
    search_entry.connect_search_changed(glib::clone!(
        #[weak]
        list_box,
        move |entry| {
            let query = entry.text().to_lowercase();
            let mut idx = 0;
            while let Some(row) = list_box.row_at_index(idx) {
                if query.is_empty() {
                    row.set_visible(true);
                } else if let Some(action_row) = row
                    .child()
                    .and_then(|c| c.downcast_ref::<adw::ActionRow>().cloned())
                {
                    let title = action_row.title().to_lowercase();
                    let subtitle = action_row
                        .subtitle()
                        .map(|s| s.to_lowercase())
                        .unwrap_or_default();
                    row.set_visible(title.contains(&query) || subtitle.contains(&query));
                } else {
                    row.set_visible(true);
                }
                idx += 1;
            }
        }
    ));

    content_box.append(&scrolled);
    toolbar_view.set_content(Some(&content_box));
    dialog.set_child(Some(&toolbar_view));
    dialog.present(Some(widget));
}
