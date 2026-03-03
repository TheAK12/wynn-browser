// ─────────────────────────────────────────────────────────────────────
// adblocker.rs  –  Ad blocking for Wynn Browser
// ─────────────────────────────────────────────────────────────────────
//
// Three layers of ad blocking:
//
//   1. **Domain-level blocking** – Pete Lowe's hosts-format blocklist
//      is fetched and cached locally.  JS overrides for `fetch()` and
//      `XMLHttpRequest.open()` silently block requests to known ad
//      domains.  A MutationObserver hides ad iframes.
//
//   2. **CSS cosmetic filters** – hide known ad containers (Google Ads,
//      Taboola, Outbrain) via UserStyleSheet.
//
//   3. **Site-specific filters** – targeted CSS and JS for:
//      - **YouTube**: hides ad overlays, companion ads, banner ads,
//        and auto-skips pre-roll/mid-roll ads (mutes + speeds through
//        + clicks skip button).
//      - **Spotify**: hides display ads, upgrade nags, and sponsored
//        content in the web player.
//
// Blocklist source:
//
//   https://pgl.yoyo.org/adservers/serverlist.php?hostformat=hosts
//       &showintro=0&mimetype=plaintext
//
// Cached at:  ~/.local/share/wynn-browser/blocklist.txt
// ─────────────────────────────────────────────────────────────────────

use std::cell::RefCell;
use std::collections::HashSet;
use std::fs;
use std::io::{BufRead, BufReader};
use std::path::PathBuf;

use webkit6::{
    UserContentFilterStore, UserContentInjectedFrames, UserContentManager, UserScript,
    UserScriptInjectionTime, UserStyleLevel, UserStyleSheet,
};

use crate::database;
use crate::webview;

// ── Blocklist URL ───────────────────────────────────────────────────

const BLOCKLIST_URL: &str =
    "https://pgl.yoyo.org/adservers/serverlist.php?hostformat=hosts&showintro=0&mimetype=plaintext";

// ── Thread-local domain set ─────────────────────────────────────────

thread_local! {
    static BLOCKED_DOMAINS: RefCell<HashSet<String>> = RefCell::new(HashSet::new());
}

/// Path to the locally cached blocklist file.
fn blocklist_path() -> PathBuf {
    database::data_dir().join("blocklist.txt")
}

// ── Blocklist parsing ───────────────────────────────────────────────

/// Parse a hosts-format blocklist into a set of domain strings.
///
/// Expected line format:  `127.0.0.1 ad.example.com`
/// Lines starting with `#` or empty lines are skipped.
fn parse_hosts_file(content: &str) -> HashSet<String> {
    let mut domains = HashSet::new();
    for line in content.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        // Format: "127.0.0.1 domain" or "0.0.0.0 domain"
        let parts: Vec<&str> = trimmed.split_whitespace().collect();
        if parts.len() >= 2 {
            let domain = parts[1].to_lowercase();
            // Skip localhost entries.
            if domain != "localhost"
                && domain != "localhost.localdomain"
                && domain != "broadcasthost"
                && domain != "local"
                && !domain.is_empty()
            {
                domains.insert(domain);
            }
        }
    }
    domains
}

/// Load the blocklist from the local cache file.
fn load_cached_blocklist() -> HashSet<String> {
    let path = blocklist_path();
    if !path.exists() {
        return HashSet::new();
    }

    match fs::File::open(&path) {
        Ok(file) => {
            let reader = BufReader::new(file);
            let mut domains = HashSet::new();
            for line in reader.lines() {
                if let Ok(line) = line {
                    let trimmed = line.trim().to_string();
                    if !trimmed.is_empty() && !trimmed.starts_with('#') {
                        let parts: Vec<&str> = trimmed.split_whitespace().collect();
                        if parts.len() >= 2 {
                            let domain = parts[1].to_lowercase();
                            if domain != "localhost" && !domain.is_empty() {
                                domains.insert(domain);
                            }
                        }
                    }
                }
            }
            domains
        }
        Err(_) => HashSet::new(),
    }
}

/// Save raw blocklist content to the cache file.
fn save_blocklist_cache(content: &str) {
    let path = blocklist_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(path, content);
}

/// Initialise the thread-local domain set from the cached file.
fn ensure_domains_loaded() {
    BLOCKED_DOMAINS.with(|cell| {
        let mut set = cell.borrow_mut();
        if set.is_empty() {
            *set = load_cached_blocklist();
        }
    });
}

// ── CSS cosmetic filters ────────────────────────────────────────────

// Cosmetic CSS filters.
//
// IMPORTANT: Only target selectors that are unambiguously ad-related.
// Avoid substring attribute selectors like [class*="ad-"] because
// they false-positive on words containing "ad" (e.g. "download",
// "header", "loading", "thread", "upload", "navigation", "shadow",
// "pad", "road", "read", etc.).  Use exact class/id names or
// prefix-based selectors that are tied to specific ad networks.
const AD_HIDING_CSS: &str = r#"
/* Google Ads */
ins.adsbygoogle,
.adsbygoogle,
[id^="google_ads_iframe"],
[id^="div-gpt-ad"],

/* Common ad network containers (exact or prefix matches only) */
[id^="taboola-"],
.taboola-widget,
[id^="outbrain_widget"],
.OUTBRAIN,
.ob-widget,
.ob-widget-items-container
{
    display: none !important;
    visibility: hidden !important;
    height: 0 !important;
    max-height: 0 !important;
    overflow: hidden !important;
    pointer-events: none !important;
}
"#;

// ── JavaScript domain blocker ───────────────────────────────────────

/// Generate the JS blocker script with the current domain set embedded.
///
/// Strategy:
///   - Override `fetch()` and `XMLHttpRequest.open()` to silently block
///     requests to known ad/tracking domains.
///   - Use a MutationObserver to hide (not remove) `<iframe>` elements
///     whose `src` points to a blocked domain.  Iframes are the primary
///     vehicle for display ads.
///   - Do NOT touch `<img>`, `<script>`, or `<link>` elements — removing
///     those breaks page layout (logos, icons), JS frameworks, and
///     stylesheets.  The domain blocklist already prevents the *network
///     request*; the element just won't load.
///   - Never block first-party (same-site) requests.
fn generate_blocker_js() -> String {
    let domains: Vec<String> = BLOCKED_DOMAINS.with(|cell| cell.borrow().iter().cloned().collect());

    // If no domains loaded, use a minimal hardcoded set.
    let domain_array = if domains.is_empty() {
        // Fallback minimal set — only unambiguous ad domains.
        vec![
            "doubleclick.net".to_string(),
            "googlesyndication.com".to_string(),
            "googleadservices.com".to_string(),
            "adnxs.com".to_string(),
            "advertising.com".to_string(),
            "outbrain.com".to_string(),
            "taboola.com".to_string(),
            "amazon-adsystem.com".to_string(),
        ]
    } else {
        domains
    };

    // Build a JS Set of blocked domains for O(1) lookup.
    let js_domains: String = domain_array
        .iter()
        .map(|d| format!("\"{}\"", d.replace('"', "\\\"").replace('\\', "\\\\")))
        .collect::<Vec<_>>()
        .join(",");

    format!(
        r#"(function() {{
    'use strict';

    const blockedDomains = new Set([{domains}]);
    const pageDomain = location.hostname.toLowerCase();

    // Extract the registrable domain (eTLD+1 approximation).
    function getBaseDomain(hostname) {{
        const parts = hostname.split('.');
        if (parts.length <= 2) return hostname;
        return parts.slice(-2).join('.');
    }}

    const pageBase = getBaseDomain(pageDomain);

    function isDomainBlocked(hostname) {{
        if (!hostname) return false;
        hostname = hostname.toLowerCase();
        // Never block first-party requests.
        if (hostname === pageDomain) return false;
        if (getBaseDomain(hostname) === pageBase) return false;
        // Exact match.
        if (blockedDomains.has(hostname)) return true;
        // Check parent domains (e.g. sub.ad.com → ad.com).
        const parts = hostname.split('.');
        for (let i = 1; i < parts.length - 1; i++) {{
            if (blockedDomains.has(parts.slice(i).join('.'))) return true;
        }}
        return false;
    }}

    function isUrlBlocked(url) {{
        if (!url) return false;
        try {{
            const u = new URL(url, location.href);
            // Only block http/https — never block data:, blob:, etc.
            if (u.protocol !== 'http:' && u.protocol !== 'https:') return false;
            return isDomainBlocked(u.hostname);
        }} catch (e) {{
            return false;
        }}
    }}

    // Hide (don't remove) iframes pointing to ad domains.
    // Removing elements can break page JS that references them.
    function hideBlockedIframes() {{
        document.querySelectorAll('iframe[src]').forEach(function(el) {{
            if (el.dataset.wynnChecked) return;
            el.dataset.wynnChecked = '1';
            if (isUrlBlocked(el.src)) {{
                el.style.setProperty('display', 'none', 'important');
                el.style.setProperty('height', '0', 'important');
                el.style.setProperty('width', '0', 'important');
                el.removeAttribute('src');
            }}
        }});
    }}

    // Block fetch() to ad domains.
    const origFetch = window.fetch;
    window.fetch = function(input, init) {{
        try {{
            const url = (typeof input === 'string') ? input : (input && input.url);
            if (isUrlBlocked(url)) {{
                return new Response('', {{ status: 200, statusText: 'Blocked' }});
            }}
        }} catch(e) {{}}
        return origFetch.apply(this, arguments);
    }};

    // Block XMLHttpRequest to ad domains.
    const origOpen = XMLHttpRequest.prototype.open;
    XMLHttpRequest.prototype.open = function(method, url) {{
        if (typeof url === 'string' && isUrlBlocked(url)) {{
            // Redirect to an empty data URI instead of the ad server.
            return origOpen.call(this, method, 'data:text/plain,');
        }}
        return origOpen.apply(this, arguments);
    }};

    // Initial pass.
    if (document.readyState === 'loading') {{
        document.addEventListener('DOMContentLoaded', hideBlockedIframes);
    }} else {{
        hideBlockedIframes();
    }}

    // Observe for dynamically inserted iframes only.
    // Debounce so we don't fire on every tiny DOM change.
    let debounceTimer = null;
    const observer = new MutationObserver(function() {{
        if (debounceTimer) return;
        debounceTimer = setTimeout(function() {{
            debounceTimer = null;
            hideBlockedIframes();
        }}, 250);
    }});
    observer.observe(document.documentElement, {{
        childList: true,
        subtree: true,
    }});
}})();
"#,
        domains = js_domains
    )
}

// ── Site-specific: YouTube ───────────────────────────────────────────

/// CSS to hide YouTube ad UI elements.
///
/// These selectors target the ad overlay, companion ads, banner ads,
/// promoted content, and the ad container that wraps pre-roll/mid-roll
/// video ads.  The actual video stream cannot be blocked (it comes
/// from the same googlevideo.com CDN as regular content), but hiding
/// the UI and auto-skipping (via JS below) significantly improves UX.
const YOUTUBE_AD_CSS: &str = r#"
/* Pre-roll / mid-roll ad overlay & container */
.video-ads,
.ytp-ad-module,
.ytp-ad-overlay-container,
.ytp-ad-overlay-slot,
.ytp-ad-text-overlay,
.ytp-ad-image-overlay,

/* Skip button area — we auto-click it via JS, but hide any residual */
.ytp-ad-skip-button-modern,
.ytp-ad-preview-container,

/* Companion ads (sidebar ads that appear alongside video) */
#companion,
#player-ads,
ytd-companion-slot-renderer,
ytd-action-companion-ad-renderer,
ytd-promoted-sparkles-web-renderer,

/* Banner ads and display ads in feed */
ytd-banner-promo-renderer,
ytd-statement-banner-renderer,
ytd-in-feed-ad-layout-renderer,
ytd-ad-slot-renderer,
ytd-display-ad-renderer,
ytd-promoted-video-renderer,
ytd-carousel-ad-renderer,

/* Masthead ad (homepage hero ad) */
#masthead-ad,
ytd-primetime-promo-renderer,

/* "Ad" badge and labels */
.ytp-ad-badge,
.ytp-ad-visit-advertiser-button,

/* Merch shelf and paid promotions */
ytd-merch-shelf-renderer,
.ytd-promoted-sparkles-text-search-renderer,

/* Survey / feedback ads */
.ytp-ad-survey,
tp-yt-paper-dialog.ytd-popup-container
{
    display: none !important;
    visibility: hidden !important;
    height: 0 !important;
    max-height: 0 !important;
    overflow: hidden !important;
    pointer-events: none !important;
}

/* Make the ad player progress bar invisible */
.ytp-ad-progress-list {
    display: none !important;
}
"#;

/// JavaScript for YouTube ad mitigation.
///
/// Strategy:
///   1. Detect when a pre-roll or mid-roll ad is playing by checking
///      for the `.ad-showing` class on the player.
///   2. When an ad is detected:
///      a. Mute the video (restore volume after).
///      b. Set playback rate to maximum (16x) to burn through the ad.
///      c. Click the skip button as soon as it appears.
///   3. Poll every 500ms — lightweight and reliable.
const YOUTUBE_AD_JS: &str = r#"(function() {
    'use strict';

    let savedVolume = -1;

    function trySkipAd() {
        const player = document.querySelector('.html5-video-player');
        if (!player) return;

        const isAd = player.classList.contains('ad-showing') ||
                     player.classList.contains('ad-interrupting');

        if (isAd) {
            const video = player.querySelector('video');
            if (video) {
                // Save volume on first detection, then mute.
                if (savedVolume < 0) {
                    savedVolume = video.volume;
                }
                video.volume = 0;

                // Speed through the ad as fast as possible.
                try { video.playbackRate = 16; } catch(e) {}

                // If we can determine the ad duration, skip to end.
                if (video.duration && isFinite(video.duration)) {
                    video.currentTime = video.duration;
                }
            }

            // Click the skip button if available.
            const skipSelectors = [
                '.ytp-skip-ad-button',
                '.ytp-ad-skip-button',
                '.ytp-ad-skip-button-modern',
                'button.ytp-ad-skip-button-modern',
                '.ytp-skip-ad-button__text',
                '[id="skip-button:a"] button',
                '[id="skip-button:b"] button'
            ];
            for (const sel of skipSelectors) {
                const btn = document.querySelector(sel);
                if (btn) {
                    btn.click();
                    break;
                }
            }

            // Also dismiss any overlay ads.
            const closeOverlay = document.querySelector('.ytp-ad-overlay-close-button');
            if (closeOverlay) closeOverlay.click();
        } else {
            // Ad finished — restore volume.
            if (savedVolume >= 0) {
                const video = player.querySelector('video');
                if (video) {
                    video.volume = savedVolume;
                    try { video.playbackRate = 1; } catch(e) {}
                }
                savedVolume = -1;
            }
        }
    }

    // Poll for ads.  MutationObserver on class changes is less
    // reliable here because YouTube's player mutates heavily.
    setInterval(trySkipAd, 500);

    // Also run on page navigation (YouTube is a SPA).
    const origPushState = history.pushState;
    history.pushState = function() {
        origPushState.apply(this, arguments);
        setTimeout(trySkipAd, 1000);
    };
})();"#;

// ── Site-specific: Spotify ──────────────────────────────────────────

/// CSS to hide Spotify web player display/text ads.
///
/// Spotify serves audio ads from first-party domains so those cannot
/// be blocked.  But the visual ad slots, upgrade nag banners, and
/// sponsored content labels can be hidden.
const SPOTIFY_AD_CSS: &str = r#"
/* Upgrade / Premium upsell banners */
.upgrade-cta,
[data-testid="upgrade-button"],
[data-testid="upgrade-menu-item"],
.main-premiumTrialBanner-container,

/* Sponsored / ad slots in browse and playlists */
[data-testid="ad-slot-container"],
.sponsor-container,
.desktopLayoutRightSidebar [aria-label="Sponsored"],

/* Leaderboard / display ad iframe at bottom */
.main-leaderboardAd-container,
iframe[src*="spclient"],

/* "Sponsored" labels and recommendation cards that are ads */
[aria-label="Sponsored"],
[data-testid="hpto-banner"],

/* Free-tier limitations nag */
.main-shuffleButton-disabled-overlay
{
    display: none !important;
    visibility: hidden !important;
    height: 0 !important;
    max-height: 0 !important;
    overflow: hidden !important;
    pointer-events: none !important;
}
"#;

/// Create a new [`UserContentManager`], optionally pre-loaded with
/// ad-blocking CSS and JavaScript rules.
pub fn create_content_manager(enabled: bool) -> UserContentManager {
    let ucm = UserContentManager::new();

    if enabled {
        ensure_domains_loaded();
        load_rules(&ucm);
    }

    ucm
}

/// Load ad-blocking rules into the given content manager.
///
/// This loads both:
///   1. JS/CSS injection rules (domain blocker, cosmetic filters, YouTube/Spotify).
///   2. Native WebKit content filter (Safari-compatible JSON rules) for
///      engine-level sub-resource blocking.
pub fn load_rules(ucm: &UserContentManager) {
    ensure_domains_loaded();

    // ── Global CSS cosmetic filters ─────────────────────────────────
    let style = UserStyleSheet::new(
        AD_HIDING_CSS,
        UserContentInjectedFrames::AllFrames,
        UserStyleLevel::User,
        &[],                    // allow list – empty means all sites
        webview::CF_BLOCK_LIST, // block list – skip Cloudflare challenge frames
    );
    ucm.add_style_sheet(&style);

    // ── Global JS domain blocker ────────────────────────────────────
    let js = generate_blocker_js();
    let script = UserScript::new(
        &js,
        UserContentInjectedFrames::AllFrames,
        UserScriptInjectionTime::Start,
        &[],
        webview::CF_BLOCK_LIST, // block list – skip Cloudflare challenge frames
    );
    ucm.add_script(&script);

    // ── YouTube-specific CSS ────────────────────────────────────────
    let yt_css = UserStyleSheet::new(
        YOUTUBE_AD_CSS,
        UserContentInjectedFrames::AllFrames,
        UserStyleLevel::User,
        &["https://*.youtube.com/*", "https://youtube.com/*"],
        &[],
    );
    ucm.add_style_sheet(&yt_css);

    // ── YouTube-specific JS (ad skip + mute) ────────────────────────
    let yt_js = UserScript::new(
        YOUTUBE_AD_JS,
        UserContentInjectedFrames::TopFrame,
        UserScriptInjectionTime::End, // Run after page load so player exists.
        &["https://*.youtube.com/*", "https://youtube.com/*"],
        &[],
    );
    ucm.add_script(&yt_js);

    // ── Spotify-specific CSS ────────────────────────────────────────
    let sp_css = UserStyleSheet::new(
        SPOTIFY_AD_CSS,
        UserContentInjectedFrames::AllFrames,
        UserStyleLevel::User,
        &["https://*.spotify.com/*", "https://spotify.com/*"],
        &[],
    );
    ucm.add_style_sheet(&sp_css);

    // ── Native WebKit content filter (engine-level blocking) ────────
    apply_native_filter(ucm);
}

/// Remove all ad-blocking rules from the content manager.
pub fn clear_rules(ucm: &UserContentManager) {
    ucm.remove_all_style_sheets();
    ucm.remove_all_scripts();
    ucm.remove_all_filters();
}

/// Check whether ad blocking is enabled (reads from settings DB).
pub fn is_enabled() -> bool {
    database::with_db(|conn| {
        conn.query_row(
            "SELECT value FROM settings WHERE key = 'adblock_enabled'",
            [],
            |row| row.get::<_, String>(0),
        )
        .unwrap_or_else(|_| "true".to_string())
            == "true"
    })
}

/// Persist the ad-blocking enabled state to the database.
pub fn set_enabled(enabled: bool) {
    let value = if enabled { "true" } else { "false" };
    database::with_db(|conn| {
        let _ = conn.execute(
            "INSERT OR REPLACE INTO settings (key, value) VALUES ('adblock_enabled', ?1)",
            rusqlite::params![value],
        );
    });
}

/// Refresh the blocklist by fetching Pete Lowe's list using
/// a subprocess call to `curl` (avoids adding HTTP crate deps).
///
/// This runs synchronously and should be called from a background
/// context or at startup.  Returns the number of domains loaded.
pub fn refresh_blocklist() -> usize {
    // Use std::process::Command to fetch the list via curl.
    let output = std::process::Command::new("curl")
        .args([
            "-sL",
            "--connect-timeout",
            "10",
            "--max-time",
            "30",
            BLOCKLIST_URL,
        ])
        .output();

    match output {
        Ok(result) if result.status.success() => {
            let content = String::from_utf8_lossy(&result.stdout).to_string();
            let domains = parse_hosts_file(&content);
            let count = domains.len();

            // Cache to disk.
            save_blocklist_cache(&content);

            // Update thread-local set.
            BLOCKED_DOMAINS.with(|cell| {
                *cell.borrow_mut() = domains;
            });

            count
        }
        _ => {
            // If curl fails, try wget as fallback.
            let output = std::process::Command::new("wget")
                .args(["-qO-", "--timeout=10", BLOCKLIST_URL])
                .output();

            match output {
                Ok(result) if result.status.success() => {
                    let content = String::from_utf8_lossy(&result.stdout).to_string();
                    let domains = parse_hosts_file(&content);
                    let count = domains.len();
                    save_blocklist_cache(&content);
                    BLOCKED_DOMAINS.with(|cell| {
                        *cell.borrow_mut() = domains;
                    });
                    count
                }
                _ => 0, // Failed to fetch.
            }
        }
    }
}

/// Return the number of blocked domains currently loaded.
pub fn blocklist_domain_count() -> usize {
    ensure_domains_loaded();
    BLOCKED_DOMAINS.with(|cell| cell.borrow().len())
}

// ── Native WebKit content filter (Safari-compatible JSON rules) ─────
//
// WebKit's UserContentFilter system compiles JSON rules into an
// efficient bytecode format and evaluates them in the network layer,
// blocking sub-resource requests *before* they're sent.  This is
// far more performant than the JS-based fetch/XHR override approach.
//
// The JS injection blocker is kept as a complementary layer because:
//   - It catches dynamically-created iframes.
//   - It handles YouTube/Spotify site-specific mitigations.
//   - The native filter handles everything else (images, scripts, XHR,
//     sub-documents, fonts, etc.) from blocked domains.

/// Path to the compiled content filter storage directory.
fn filter_store_path() -> String {
    database::data_dir()
        .join("content-filters")
        .to_string_lossy()
        .to_string()
}

/// Generate Safari-compatible content blocker JSON from the domain set.
///
/// Each rule has:
///   - `trigger.url-filter`: regex matching the domain
///   - `trigger.load-type`: ["third-party"] to avoid blocking first-party
///   - `trigger.resource-type`: all sub-resource types
///   - `action.type`: "block"
///
/// The JSON spec is documented at:
///   https://developer.apple.com/documentation/safariservices/creating-a-content-blocker
fn generate_content_blocker_json() -> String {
    let domains: Vec<String> = BLOCKED_DOMAINS.with(|cell| cell.borrow().iter().cloned().collect());

    if domains.is_empty() {
        return "[]".to_string();
    }

    // Cloudflare domains that must never be blocked — Turnstile
    // challenges load resources from these and blocking them causes
    // the human verification to get stuck.
    let cf_whitelist: HashSet<&str> = [
        "challenges.cloudflare.com",
        "cloudflare.com",
        "cloudflareinsights.com",
        "static.cloudflareinsights.com",
        "cdnjs.cloudflare.com",
        "cloudflare-dns.com",
    ]
    .into_iter()
    .collect();

    // WebKit has a limit on the number of rules (~75,000 in practice).
    // We batch domains into groups to create broader regex patterns.
    // Each rule uses a url-filter that matches the domain in the URL.
    let mut rules = Vec::new();

    for domain in &domains {
        // Skip Cloudflare domains.
        if cf_whitelist.contains(domain.as_str()) {
            continue;
        }
        // Also skip if the domain is a subdomain of a whitelisted domain.
        let is_cf_sub = cf_whitelist
            .iter()
            .any(|cf| domain.ends_with(&format!(".{cf}")));
        if is_cf_sub {
            continue;
        }

        // Escape dots for regex.
        let escaped = domain.replace('.', "\\\\.");

        // Build a rule that blocks third-party requests to this domain.
        let rule = format!(
            r#"{{"trigger":{{"url-filter":"^https?://([^/]*\\.)?{domain}","load-type":["third-party"]}},"action":{{"type":"block"}}}}"#,
            domain = escaped
        );
        rules.push(rule);
    }

    format!("[{}]", rules.join(","))
}

/// Compile and apply native WebKit content blocker rules to the UCM.
///
/// This uses `UserContentFilterStore` to compile the JSON rules into
/// WebKit's efficient bytecode format, then adds the resulting
/// `UserContentFilter` to the `UserContentManager`.
///
/// The compilation is asynchronous (callback-based) — the filter will
/// be applied as soon as compilation finishes.
pub fn apply_native_filter(ucm: &UserContentManager) {
    ensure_domains_loaded();

    let domain_count = BLOCKED_DOMAINS.with(|cell| cell.borrow().len());
    if domain_count == 0 {
        return;
    }

    let json = generate_content_blocker_json();
    let store_path = filter_store_path();

    // Ensure the storage directory exists.
    let _ = fs::create_dir_all(&store_path);

    let store = UserContentFilterStore::new(&store_path);
    let source_bytes = glib::Bytes::from(json.as_bytes());

    let ucm_clone = ucm.clone();

    // Try to load a previously compiled filter first (fast path).
    let store_for_load = store.clone();
    let ucm_for_load = ucm.clone();
    let store_for_save = store.clone();
    let source_for_save = source_bytes.clone();

    store_for_load.load(
        "wynn-adblock",
        gio::Cancellable::NONE,
        move |result| {
            match result {
                Ok(filter) => {
                    // Successfully loaded cached filter.
                    ucm_for_load.add_filter(&filter);
                    eprintln!("[adblocker] Loaded cached native content filter");
                }
                Err(_) => {
                    // No cached filter — compile from JSON.
                    eprintln!("[adblocker] Compiling native content filter from {} domains...", domain_count);

                    store_for_save.save(
                        "wynn-adblock",
                        &source_for_save,
                        gio::Cancellable::NONE,
                        move |result| {
                            match result {
                                Ok(filter) => {
                                    ucm_clone.add_filter(&filter);
                                    eprintln!(
                                        "[adblocker] Native content filter compiled and applied ({} domains)",
                                        domain_count
                                    );
                                }
                                Err(e) => {
                                    eprintln!("[adblocker] Failed to compile content filter: {}", e);
                                }
                            }
                        },
                    );
                }
            }
        },
    );
}

/// Remove the native content filter and recompile fresh rules.
///
/// Call this after `refresh_blocklist()` to update the compiled filter
/// with the latest domain set.
pub fn recompile_native_filter(ucm: &UserContentManager) {
    // Remove old filter.
    ucm.remove_all_filters();

    // Delete the cached compiled filter so it's rebuilt.
    let store_path = filter_store_path();
    let store = UserContentFilterStore::new(&store_path);
    store.remove("wynn-adblock", gio::Cancellable::NONE, |_result| {
        // Ignore errors — filter may not exist yet.
    });

    // Recompile and apply.
    apply_native_filter(ucm);
}
