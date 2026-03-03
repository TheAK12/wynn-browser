// ---------------------------------------------------------------------
// keepassxc.rs  --  KeePassXC browser protocol client for Wynn Browser
// ---------------------------------------------------------------------
//
// Implements the KeePassXC browser integration protocol over Unix
// sockets.  Uses NaCl crypto_box (Curve25519-XSalsa20-Poly1305) for
// encrypted communication.
//
// Protocol docs:
//   https://github.com/keepassxreboot/keepassxc-browser/blob/develop/keepassxc-protocol.md
//
// Public API:
//
//   is_available()                        -- check if KeePassXC socket exists
//   get_logins(url) -> Vec<KpxcEntry>     -- fetch credentials for a URL
//   save_login(url, username, password)   -- save a credential
//   has_logins_for(url) -> bool           -- check if logins exist
//   is_connected() -> bool                -- check connection status
//   show_status_toast(widget)             -- show connection status
// ---------------------------------------------------------------------

use std::cell::RefCell;
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine;
use crypto_box::aead::OsRng;
use crypto_box::{PublicKey, SalsaBox, SecretKey};
use rand::RngCore;
use serde_json::{json, Value};

use crate::settings as app_settings;

// ── Data types ──────────────────────────────────────────────────────

/// A credential entry returned by KeePassXC.
#[derive(Debug, Clone)]
pub struct KpxcEntry {
    pub name: String,
    pub login: String,
    pub password: String,
    pub uuid: String,
}

/// Session state for an active KeePassXC connection.
struct KpxcSession {
    stream: UnixStream,
    client_secret: SecretKey,
    client_public: PublicKey,
    host_public: PublicKey,
    client_id: String,            // base64-encoded 24-byte random ID
    association_id: String,       // database identifier from associate
    id_public_key: String,        // base64-encoded identification public key
    id_secret_key_bytes: Vec<u8>, // raw identification secret key (stored)
}

// ── Thread-local session cache ──────────────────────────────────────

thread_local! {
    static SESSION: RefCell<Option<KpxcSession>> = const { RefCell::new(None) };
}

// ── Socket discovery ────────────────────────────────────────────────

/// Find the KeePassXC browser server Unix socket path.
fn socket_path() -> Option<String> {
    let runtime_dir = std::env::var("XDG_RUNTIME_DIR").ok()?;

    // Primary path (KeePassXC 2.7+).
    let primary = format!(
        "{}/app/org.keepassxc.KeePassXC/org.keepassxc.KeePassXC.BrowserServer",
        runtime_dir
    );
    if std::path::Path::new(&primary).exists() {
        return Some(primary);
    }

    // Legacy symlink path.
    let legacy = format!("{}/org.keepassxc.KeePassXC.BrowserServer", runtime_dir);
    if std::path::Path::new(&legacy).exists() {
        return Some(legacy);
    }

    // Flatpak path.
    let flatpak = format!(
        "{}/app/org.keepassxc.KeePassXC/org.keepassxc.KeePassXC.BrowserServer",
        runtime_dir
    );
    if std::path::Path::new(&flatpak).exists() {
        return Some(flatpak);
    }

    None
}

/// Check whether KeePassXC is available (socket exists).
pub fn is_available() -> bool {
    socket_path().is_some()
}

/// Check whether we have an active session.
pub fn is_connected() -> bool {
    SESSION.with(|s| s.borrow().is_some())
}

// ── Low-level socket I/O ────────────────────────────────────────────

fn send_json(stream: &mut UnixStream, val: &Value) -> Result<(), String> {
    let bytes = serde_json::to_vec(val).map_err(|e| format!("JSON encode: {e}"))?;
    stream
        .write_all(&bytes)
        .map_err(|e| format!("Socket write: {e}"))?;
    stream.flush().map_err(|e| format!("Socket flush: {e}"))?;
    Ok(())
}

fn read_json(stream: &mut UnixStream) -> Result<Value, String> {
    let mut buf = Vec::with_capacity(8192);
    let mut tmp = [0u8; 8192];

    // Set a read timeout so we don't hang forever.
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| format!("Set timeout: {e}"))?;

    loop {
        match stream.read(&mut tmp) {
            Ok(0) => return Err("Connection closed".into()),
            Ok(n) => {
                buf.extend_from_slice(&tmp[..n]);
                if let Ok(val) = serde_json::from_slice::<Value>(&buf) {
                    return Ok(val);
                }
                // Incomplete JSON — keep reading.
            }
            Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                // Timeout — try to parse what we have.
                if !buf.is_empty() {
                    return serde_json::from_slice::<Value>(&buf)
                        .map_err(|e| format!("Incomplete response: {e}"));
                }
                return Err("Read timeout".into());
            }
            Err(e) => return Err(format!("Socket read: {e}")),
        }
    }
}

// ── NaCl crypto helpers ─────────────────────────────────────────────

fn random_24_bytes() -> [u8; 24] {
    let mut buf = [0u8; 24];
    OsRng.fill_bytes(&mut buf);
    buf
}

fn increment_nonce(nonce: &[u8; 24]) -> [u8; 24] {
    let mut result = *nonce;
    for byte in result.iter_mut() {
        let (val, overflow) = byte.overflowing_add(1);
        *byte = val;
        if !overflow {
            break;
        }
    }
    result
}

fn encrypt_message(
    inner: &Value,
    host_pk: &PublicKey,
    client_sk: &SecretKey,
) -> Result<(String, String), String> {
    let nonce_bytes = random_24_bytes();
    let nonce = crypto_box::Nonce::from(nonce_bytes);
    let salsa = SalsaBox::new(host_pk, client_sk);
    let plaintext = serde_json::to_vec(inner).map_err(|e| format!("JSON: {e}"))?;

    use crypto_box::aead::Aead;
    let ciphertext = salsa
        .encrypt(&nonce, plaintext.as_ref())
        .map_err(|e| format!("Encrypt: {e}"))?;

    Ok((B64.encode(&ciphertext), B64.encode(nonce_bytes)))
}

fn decrypt_message(
    message_b64: &str,
    request_nonce: &[u8; 24],
    host_pk: &PublicKey,
    client_sk: &SecretKey,
) -> Result<Value, String> {
    let ciphertext = B64
        .decode(message_b64)
        .map_err(|e| format!("Base64 decode: {e}"))?;

    let response_nonce_bytes = increment_nonce(request_nonce);
    let nonce = crypto_box::Nonce::from(response_nonce_bytes);
    let salsa = SalsaBox::new(host_pk, client_sk);

    use crypto_box::aead::Aead;
    let plaintext = salsa
        .decrypt(&nonce, ciphertext.as_ref())
        .map_err(|_| "Decryption failed — wrong key or corrupt message".to_string())?;

    serde_json::from_slice(&plaintext).map_err(|e| format!("JSON parse: {e}"))
}

// ── Persistent association storage ──────────────────────────────────

fn save_association(db_id: &str, id_public_b64: &str, id_secret_bytes: &[u8]) {
    app_settings::set_setting("kpxc_association_id", db_id);
    app_settings::set_setting("kpxc_id_public_key", id_public_b64);
    app_settings::set_setting("kpxc_id_secret_key", &B64.encode(id_secret_bytes));
}

fn load_association() -> Option<(String, String, Vec<u8>)> {
    let db_id = app_settings::get_setting("kpxc_association_id")?;
    let id_public = app_settings::get_setting("kpxc_id_public_key")?;
    let id_secret_b64 = app_settings::get_setting("kpxc_id_secret_key")?;

    if db_id.is_empty() || id_public.is_empty() || id_secret_b64.is_empty() {
        return None;
    }

    let id_secret = B64.decode(&id_secret_b64).ok()?;
    Some((db_id, id_public, id_secret))
}

// ── Connection and key exchange ─────────────────────────────────────

/// Connect to KeePassXC and perform key exchange + association.
/// Returns an error message on failure.
pub fn connect() -> Result<(), String> {
    let path = socket_path().ok_or(
        "KeePassXC socket not found. Is KeePassXC running with browser integration enabled?",
    )?;

    let mut stream =
        UnixStream::connect(&path).map_err(|e| format!("Cannot connect to KeePassXC: {e}"))?;

    // Generate ephemeral session keypair.
    let client_secret = SecretKey::generate(&mut OsRng);
    let client_public = client_secret.public_key();

    // Generate random clientID.
    let client_id_bytes = random_24_bytes();
    let client_id = B64.encode(client_id_bytes);

    // Step 1: change-public-keys (unencrypted).
    let nonce_bytes = random_24_bytes();
    let kx_msg = json!({
        "action": "change-public-keys",
        "publicKey": B64.encode(client_public.as_bytes()),
        "nonce": B64.encode(nonce_bytes),
        "clientID": &client_id,
    });

    send_json(&mut stream, &kx_msg)?;
    let kx_resp = read_json(&mut stream)?;

    if kx_resp.get("success").and_then(|v| v.as_str()) != Some("true") {
        return Err("Key exchange rejected by KeePassXC".into());
    }

    let host_pk_b64 = kx_resp
        .get("publicKey")
        .and_then(|v| v.as_str())
        .ok_or("No publicKey in response")?;
    let host_pk_bytes = B64
        .decode(host_pk_b64)
        .map_err(|_| "Invalid host public key")?;
    if host_pk_bytes.len() != 32 {
        return Err("Invalid host public key length".into());
    }
    let mut pk_arr = [0u8; 32];
    pk_arr.copy_from_slice(&host_pk_bytes);
    let host_public = PublicKey::from(pk_arr);

    // Step 2: Try test-associate with saved association.
    let (assoc_id, id_pub_b64, id_sec_bytes) =
        if let Some((saved_id, saved_pub, saved_sec)) = load_association() {
            // Try test-associate.
            let inner = json!({
                "action": "test-associate",
                "id": &saved_id,
                "key": &saved_pub,
            });
            let (msg, nonce) = encrypt_message(&inner, &host_public, &client_secret)?;
            let nonce_bytes_arr: [u8; 24] = B64
                .decode(&nonce)
                .map_err(|_| "nonce decode")?
                .try_into()
                .map_err(|_| "nonce length")?;

            let wire = json!({
                "action": "test-associate",
                "message": msg,
                "nonce": nonce,
                "clientID": &client_id,
            });
            send_json(&mut stream, &wire)?;
            let resp = read_json(&mut stream)?;

            // Check if test-associate succeeded.
            if let Some(resp_msg) = resp.get("message").and_then(|v| v.as_str()) {
                if let Ok(decrypted) =
                    decrypt_message(resp_msg, &nonce_bytes_arr, &host_public, &client_secret)
                {
                    if decrypted.get("success").and_then(|v| v.as_str()) == Some("true") {
                        // Association still valid.
                        (saved_id, saved_pub, saved_sec)
                    } else {
                        // Association invalid — need to re-associate.
                        do_associate(&mut stream, &client_id, &host_public, &client_secret)?
                    }
                } else {
                    do_associate(&mut stream, &client_id, &host_public, &client_secret)?
                }
            } else if resp.get("success").and_then(|v| v.as_str()) == Some("true") {
                // Some versions return success at top level.
                (saved_id, saved_pub, saved_sec)
            } else {
                do_associate(&mut stream, &client_id, &host_public, &client_secret)?
            }
        } else {
            // No saved association — create one.
            do_associate(&mut stream, &client_id, &host_public, &client_secret)?
        };

    // Store session.
    SESSION.with(|s| {
        *s.borrow_mut() = Some(KpxcSession {
            stream,
            client_secret,
            client_public,
            host_public,
            client_id,
            association_id: assoc_id,
            id_public_key: id_pub_b64,
            id_secret_key_bytes: id_sec_bytes,
        });
    });

    Ok(())
}

/// Perform the `associate` handshake.  KeePassXC will prompt the user.
fn do_associate(
    stream: &mut UnixStream,
    client_id: &str,
    host_pk: &PublicKey,
    client_sk: &SecretKey,
) -> Result<(String, String, Vec<u8>), String> {
    // Generate a permanent identification keypair.
    let id_secret = SecretKey::generate(&mut OsRng);
    let id_public = id_secret.public_key();
    let id_pub_b64 = B64.encode(id_public.as_bytes());
    let id_sec_bytes = id_secret.to_bytes().to_vec();

    // Also send the session public key.
    let session_pub_b64 = B64.encode(client_sk.public_key().as_bytes());

    let inner = json!({
        "action": "associate",
        "key": session_pub_b64,
        "idKey": &id_pub_b64,
    });

    let (msg, nonce) = encrypt_message(&inner, host_pk, client_sk)?;
    let nonce_bytes: [u8; 24] = B64
        .decode(&nonce)
        .map_err(|_| "nonce decode")?
        .try_into()
        .map_err(|_| "nonce length")?;

    let wire = json!({
        "action": "associate",
        "message": msg,
        "nonce": nonce,
        "clientID": client_id,
    });

    // Use a longer timeout for associate — user needs to accept.
    stream
        .set_read_timeout(Some(Duration::from_secs(30)))
        .map_err(|e| format!("Set timeout: {e}"))?;

    send_json(stream, &wire)?;
    let resp = read_json(stream)?;

    // Reset timeout.
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .map_err(|e| format!("Set timeout: {e}"))?;

    // Decrypt response.
    let resp_msg = resp
        .get("message")
        .and_then(|v| v.as_str())
        .ok_or("No message in associate response. User may have denied the request.")?;

    let decrypted = decrypt_message(resp_msg, &nonce_bytes, host_pk, client_sk)?;

    if decrypted.get("success").and_then(|v| v.as_str()) != Some("true") {
        let err = decrypted
            .get("error")
            .and_then(|v| v.as_str())
            .unwrap_or("Association denied");
        return Err(format!("KeePassXC: {err}"));
    }

    let db_id = decrypted
        .get("id")
        .and_then(|v| v.as_str())
        .unwrap_or("wynn-browser")
        .to_string();

    // Persist the association.
    save_association(&db_id, &id_pub_b64, &id_sec_bytes);

    Ok((db_id, id_pub_b64, id_sec_bytes))
}

// ── Ensure connected ────────────────────────────────────────────────

fn ensure_connected() -> Result<(), String> {
    if !is_connected() {
        connect()?;
    }
    Ok(())
}

// ── Public API: get logins ──────────────────────────────────────────

/// Fetch credentials for a URL from KeePassXC.
pub fn get_logins(url: &str) -> Result<Vec<KpxcEntry>, String> {
    ensure_connected()?;

    SESSION.with(|s| {
        let mut session_opt = s.borrow_mut();
        let session = session_opt.as_mut().ok_or("Not connected")?;

        let inner = json!({
            "action": "get-logins",
            "url": url,
            "keys": [{
                "id": &session.association_id,
                "key": &session.id_public_key,
            }],
        });

        let (msg, nonce) = encrypt_message(&inner, &session.host_public, &session.client_secret)?;
        let nonce_bytes: [u8; 24] = B64
            .decode(&nonce)
            .map_err(|_| "nonce decode")?
            .try_into()
            .map_err(|_| "nonce length")?;

        let wire = json!({
            "action": "get-logins",
            "message": msg,
            "nonce": nonce,
            "clientID": &session.client_id,
        });

        send_json(&mut session.stream, &wire)?;
        let resp = read_json(&mut session.stream)?;

        // Check for top-level error (e.g. database locked).
        if let Some(err) = resp.get("error").and_then(|v| v.as_str()) {
            if !err.is_empty() {
                return Err(format!("KeePassXC: {err}"));
            }
        }

        let resp_msg = resp
            .get("message")
            .and_then(|v| v.as_str())
            .ok_or("No message in get-logins response")?;

        let decrypted = decrypt_message(
            resp_msg,
            &nonce_bytes,
            &session.host_public,
            &session.client_secret,
        )?;

        if decrypted.get("success").and_then(|v| v.as_str()) != Some("true") {
            let err = decrypted
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("No logins found");
            return Err(format!("KeePassXC: {err}"));
        }

        let entries = decrypted
            .get("entries")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .map(|e| KpxcEntry {
                        name: e
                            .get("name")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        login: e
                            .get("login")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        password: e
                            .get("password")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                        uuid: e
                            .get("uuid")
                            .and_then(|v| v.as_str())
                            .unwrap_or("")
                            .to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(entries)
    })
}

/// Check if KeePassXC has logins for a URL.
pub fn has_logins_for(url: &str) -> bool {
    match get_logins(url) {
        Ok(entries) => !entries.is_empty(),
        Err(_) => false,
    }
}

// ── Public API: save login ──────────────────────────────────────────

/// Save a credential to KeePassXC.
pub fn save_login(url: &str, username: &str, password: &str) -> Result<(), String> {
    ensure_connected()?;

    SESSION.with(|s| {
        let mut session_opt = s.borrow_mut();
        let session = session_opt.as_mut().ok_or("Not connected")?;

        let inner = json!({
            "action": "set-login",
            "url": url,
            "submitUrl": url,
            "id": &session.association_id,
            "login": username,
            "password": password,
            "downloadFavicon": "true",
        });

        let (msg, nonce) = encrypt_message(&inner, &session.host_public, &session.client_secret)?;
        let nonce_bytes: [u8; 24] = B64
            .decode(&nonce)
            .map_err(|_| "nonce decode")?
            .try_into()
            .map_err(|_| "nonce length")?;

        let wire = json!({
            "action": "set-login",
            "message": msg,
            "nonce": nonce,
            "clientID": &session.client_id,
        });

        send_json(&mut session.stream, &wire)?;
        let resp = read_json(&mut session.stream)?;

        // Check for top-level error.
        if let Some(err) = resp.get("error").and_then(|v| v.as_str()) {
            if !err.is_empty() {
                return Err(format!("KeePassXC: {err}"));
            }
        }

        if let Some(resp_msg) = resp.get("message").and_then(|v| v.as_str()) {
            let decrypted = decrypt_message(
                resp_msg,
                &nonce_bytes,
                &session.host_public,
                &session.client_secret,
            )?;

            if decrypted.get("success").and_then(|v| v.as_str()) != Some("true") {
                let err = decrypted
                    .get("error")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Save failed");
                return Err(format!("KeePassXC: {err}"));
            }
        }

        Ok(())
    })
}

// ── Public API: disconnect ──────────────────────────────────────────

/// Drop the current session.
pub fn disconnect() {
    SESSION.with(|s| {
        *s.borrow_mut() = None;
    });
}

// ── Public API: password backend selection ──────────────────────────

/// Check if KeePassXC is the active password backend.
pub fn is_active_backend() -> bool {
    app_settings::get_setting("password_backend")
        .map(|v| v == "keepassxc")
        .unwrap_or(false) // Default: builtin
}

/// Get the current password backend name.
pub fn backend_name() -> String {
    app_settings::get_setting("password_backend").unwrap_or_else(|| "builtin".to_string())
}

/// Set the password backend ("builtin" or "keepassxc").
pub fn set_backend(backend: &str) {
    app_settings::set_setting("password_backend", backend);
    if backend != "keepassxc" {
        disconnect();
    }
}

// ── Autofill JS generation for KeePassXC entries ────────────────────

/// Generate autofill JavaScript for the given KeePassXC entry.
/// Uses the same MutationObserver approach as the builtin backend.
pub fn autofill_js_for_entry(entry: &KpxcEntry) -> String {
    let user_escaped = entry
        .login
        .replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('\n', "\\n")
        .replace('\r', "\\r");
    let pw_escaped = entry
        .password
        .replace('\\', "\\\\")
        .replace('\'', "\\'")
        .replace('\n', "\\n")
        .replace('\r', "\\r");

    format!(
        r#"(function() {{
    'use strict';
    if (window.__wynnAutofillActive === location.href) return;
    window.__wynnAutofillActive = location.href;

    var USERNAME = '{user}';
    var PASSWORD = '{pw}';

    var nativeSetter = Object.getOwnPropertyDescriptor(
        HTMLInputElement.prototype, 'value'
    ).set;

    function fillField(input, value) {{
        nativeSetter.call(input, value);
        input.dispatchEvent(new Event('input', {{ bubbles: true }}));
        input.dispatchEvent(new Event('change', {{ bubbles: true }}));
    }}

    function tryFill() {{
        var pwFields = document.querySelectorAll('input[type="password"]');
        var filled = false;
        for (var i = 0; i < pwFields.length; i++) {{
            var pw = pwFields[i];
            if (pw.offsetParent === null) continue;
            fillField(pw, PASSWORD);
            filled = true;
            break;
        }}

        var userSelectors = 'input[type="text"], input[type="email"], input[type="tel"], input:not([type])';
        var userFields = document.querySelectorAll(userSelectors);
        for (var j = 0; j < userFields.length; j++) {{
            var uf = userFields[j];
            if (uf.offsetParent === null) continue;
            if (uf.type === 'hidden') continue;
            fillField(uf, USERNAME);
            break;
        }}

        return filled;
    }}

    if (!tryFill()) {{
        var schedule = [300, 600, 1000, 1500, 2500, 4000, 6000, 10000];
        var idx = 0;
        var observer = new MutationObserver(function() {{
            tryFill();
        }});
        observer.observe(document.documentElement, {{
            childList: true, subtree: true
        }});
        function retry() {{
            if (idx >= schedule.length) {{
                observer.disconnect();
                return;
            }}
            setTimeout(function() {{
                tryFill();
                idx++;
                retry();
            }}, schedule[idx]);
        }}
        retry();
        setTimeout(function() {{ observer.disconnect(); }}, 30000);
    }}
}})();"#,
        user = user_escaped,
        pw = pw_escaped
    )
}
