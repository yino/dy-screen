use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use dy_screen::browser_snapshot::{BrowserPageSnapshot, is_allowed_douyin_page_url};
use dy_screen::error::{RecorderError, Result};
use tauri::webview::NewWindowResponse;
use tauri::{AppHandle, Manager, Url, WebviewUrl, WebviewWindow, WebviewWindowBuilder};
use tokio::sync::oneshot;

use crate::room_resolution::BrowserPageDriver;

pub const DOUYIN_ACCESS_WINDOW_LABEL: &str = "douyin-access";

const BROWSER_SNAPSHOT_SCRIPT: &str = r##"
(() => {
  const marker = "self.__pace_f.push";
  const maxScripts = 32;
  const maxBytes = 2097152;
  const scripts = [];
  let totalBytes = 0;
  let snapshotOverflow = false;
  let pacePayload = false;
  const encoder = new TextEncoder();
  const pageScripts = Array.from(document.scripts || []);
  for (const script of pageScripts) {
    const text = script.textContent || "";
    if (!text.includes(marker)) continue;
    pacePayload = true;
    const bytes = encoder.encode(text).byteLength;
    if (scripts.length >= maxScripts || totalBytes + bytes > maxBytes) {
      snapshotOverflow = true;
      break;
    }
    scripts.push(text);
    totalBytes += bytes;
  }
  const title = String(document.title || "");
  const isVisible = (element) => {
    if (!(element instanceof Element)) return false;
    const style = getComputedStyle(element);
    const rect = element.getBoundingClientRect();
    return style.display !== "none"
      && style.visibility !== "hidden"
      && style.opacity !== "0"
      && rect.width > 0
      && rect.height > 0;
  };
  const challengeSelector = [
    "#verify-center",
    "#captcha-interstitial",
    "[class*='captcha_verify']",
    "[id*='captcha-verify']",
    "iframe[src*='/verifycenter/captcha/']"
  ].join(",");
  const hasChallengeNode = Array.from(document.querySelectorAll(challengeSelector)).some(isVisible);
  const challengeVisible = hasChallengeNode || title.includes("验证码中间页");
  const offlineLabels = new Set(["直播已结束", "当前直播已结束"]);
  const textWalker = document.body
    ? document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT)
    : null;
  let roomOffline = false;
  let checkedTextNodes = 0;
  while (!challengeVisible && textWalker && checkedTextNodes < 5000) {
    const textNode = textWalker.nextNode();
    if (!textNode) break;
    checkedTextNodes += 1;
    const text = String(textNode.textContent || "").replace(/\s+/g, " ").trim();
    if (offlineLabels.has(text) && isVisible(textNode.parentElement)) {
      roomOffline = true;
      break;
    }
  }
  return {
    url: String(location.href || ""),
    title: title.slice(0, 512),
    readyState: String(document.readyState || "loading"),
    markers: {
      accessRestricted: challengeVisible,
      pacePayload,
      roomOffline,
      snapshotOverflow
    },
    scripts
  };
})()
"##;

const MUTE_MEDIA_SCRIPT: &str = r##"
(() => {
  const mute = (node) => {
    if (!(node instanceof HTMLMediaElement)) return;
    node.muted = true;
    node.volume = 0;
    node.setAttribute("muted", "");
  };
  document.querySelectorAll("audio,video").forEach(mute);
  if (!window.__dyScreenMuteObserver) {
    window.__dyScreenMuteObserver = new MutationObserver((records) => {
      for (const record of records) {
        for (const node of record.addedNodes) {
          mute(node);
          if (node instanceof Element) node.querySelectorAll("audio,video").forEach(mute);
        }
      }
    });
    window.__dyScreenMuteObserver.observe(document.documentElement, { childList: true, subtree: true });
  }
})()
"##;

pub struct BrowserWindowPolicy;

impl BrowserWindowPolicy {
    pub fn allows_navigation(url: &Url) -> bool {
        if is_allowed_douyin_page_url(url.as_str()) || url.as_str() == "about:blank" {
            return true;
        }

        // WKWebView applies this callback to iframe navigation as well as the main frame.
        // Douyin's challenge page cannot render if its official captcha iframe is blocked.
        url.scheme() == "https"
            && url.host_str() == Some("rmc.bytedance.com")
            && url.path().starts_with("/verifycenter/captcha/")
    }

    pub const fn allows_new_window() -> bool {
        false
    }

    pub const fn allows_download() -> bool {
        false
    }
}

#[derive(Default)]
struct BrowserRequestTracker {
    active_generation: AtomicU64,
}

impl BrowserRequestTracker {
    fn activate(&self, generation: u64) {
        self.active_generation.store(generation, Ordering::SeqCst);
    }

    fn is_current(&self, generation: u64) -> bool {
        self.active_generation.load(Ordering::SeqCst) == generation
    }

    fn invalidate(&self) {
        self.active_generation.fetch_add(1, Ordering::SeqCst);
    }
}

#[derive(Clone)]
pub struct TauriBrowserPageDriver {
    app: AppHandle,
    data_directory: Arc<PathBuf>,
    tracker: Arc<BrowserRequestTracker>,
}

impl TauriBrowserPageDriver {
    pub fn new(app: AppHandle, data_directory: PathBuf) -> Result<Self> {
        std::fs::create_dir_all(&data_directory)
            .map_err(|_| RecorderError::BrowserSessionUnavailable)?;
        Ok(Self {
            app,
            data_directory: Arc::new(data_directory),
            tracker: Arc::new(BrowserRequestTracker::default()),
        })
    }

    fn window(&self) -> Option<WebviewWindow> {
        self.app.get_webview_window(DOUYIN_ACCESS_WINDOW_LABEL)
    }

    fn create_window(&self, target: Url) -> Result<WebviewWindow> {
        let window = WebviewWindowBuilder::new(
            &self.app,
            DOUYIN_ACCESS_WINDOW_LABEL,
            WebviewUrl::External(target),
        )
        .title("抖音访问验证")
        .inner_size(1000.0, 760.0)
        .visible(false)
        .focused(false)
        .incognito(false)
        .data_directory(self.data_directory.as_ref().clone())
        .initialization_script(MUTE_MEDIA_SCRIPT)
        .on_navigation(BrowserWindowPolicy::allows_navigation)
        .on_new_window(|_, _| {
            debug_assert!(!BrowserWindowPolicy::allows_new_window());
            NewWindowResponse::Deny
        })
        .on_download(|_, _| BrowserWindowPolicy::allows_download())
        .build()
        .map_err(|_| RecorderError::BrowserSessionUnavailable)?;
        // 页面可能尚未完成加载，后续 navigate/snapshot 仍会重复执行；这里的
        // best-effort 调用用于尽早抑制首个自动播放媒体。
        let _ = window.eval(MUTE_MEDIA_SCRIPT);
        Ok(window)
    }

    fn target_url(room_url: &str) -> Result<Url> {
        let url = Url::parse(room_url).map_err(|_| RecorderError::InvalidBrowserSnapshot)?;
        if !BrowserWindowPolicy::allows_navigation(&url)
            || url.host_str() != Some("live.douyin.com")
        {
            return Err(RecorderError::InvalidBrowserSnapshot);
        }
        Ok(url)
    }
}

#[async_trait]
impl BrowserPageDriver for TauriBrowserPageDriver {
    async fn navigate(&self, request_generation: u64, room_url: &str) -> Result<()> {
        let target = Self::target_url(room_url)?;
        self.tracker.activate(request_generation);
        if let Some(window) = self.window() {
            let result = window
                .navigate(target)
                .map_err(|_| RecorderError::BrowserSessionUnavailable);
            let _ = window.eval(MUTE_MEDIA_SCRIPT);
            result
        } else {
            self.create_window(target).map(|_| ())
        }
    }

    async fn snapshot(&self, request_generation: u64) -> Result<BrowserPageSnapshot> {
        if !self.tracker.is_current(request_generation) {
            return Err(RecorderError::BrowserRequestSuperseded);
        }
        let window = self
            .window()
            .ok_or(RecorderError::BrowserSessionUnavailable)?;
        let _ = window.eval(MUTE_MEDIA_SCRIPT);
        let tracker = self.tracker.clone();
        let (sender, receiver) = oneshot::channel();
        let sender = Arc::new(Mutex::new(Some(sender)));
        window
            .eval_with_callback(BROWSER_SNAPSHOT_SCRIPT, move |json| {
                let result = if tracker.is_current(request_generation) {
                    BrowserPageSnapshot::from_json(&json)
                } else {
                    Err(RecorderError::BrowserRequestSuperseded)
                };
                if let Ok(mut sender) = sender.lock()
                    && let Some(sender) = sender.take()
                {
                    let _ = sender.send(result);
                }
            })
            .map_err(|_| RecorderError::BrowserSessionUnavailable)?;
        receiver
            .await
            .map_err(|_| RecorderError::BrowserSessionUnavailable)?
    }

    async fn show_verification(&self) -> Result<()> {
        let window = self
            .window()
            .ok_or(RecorderError::BrowserSessionUnavailable)?;
        let _ = window.eval(MUTE_MEDIA_SCRIPT);
        window
            .show()
            .and_then(|_| window.set_focus())
            .map_err(|_| RecorderError::BrowserSessionUnavailable)
    }

    async fn hide_verification(&self) -> Result<()> {
        let window = self
            .window()
            .ok_or(RecorderError::BrowserSessionUnavailable)?;
        window
            .hide()
            .map_err(|_| RecorderError::BrowserSessionUnavailable)
    }

    async fn clear_session(&self) -> Result<()> {
        self.tracker.invalidate();
        if let Some(window) = self.window() {
            window
                .clear_all_browsing_data()
                .map_err(|_| RecorderError::BrowserSessionUnavailable)?;
        }
        Ok(())
    }

    async fn shutdown(&self) -> Result<()> {
        self.tracker.invalidate();
        if let Some(window) = self.window() {
            window
                .destroy()
                .map_err(|_| RecorderError::BrowserSessionUnavailable)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_policy_only_allows_https_douyin_pages() {
        assert!(BrowserWindowPolicy::allows_navigation(
            &Url::parse("https://live.douyin.com/703940802949").unwrap()
        ));
        assert!(BrowserWindowPolicy::allows_navigation(
            &Url::parse("https://www.douyin.com/user/example").unwrap()
        ));
        assert!(!BrowserWindowPolicy::allows_navigation(
            &Url::parse("http://live.douyin.com/703940802949").unwrap()
        ));
        assert!(!BrowserWindowPolicy::allows_navigation(
            &Url::parse("https://douyin.com.example.com/703940802949").unwrap()
        ));
        assert!(!BrowserWindowPolicy::allows_new_window());
        assert!(!BrowserWindowPolicy::allows_download());
    }

    #[test]
    fn window_policy_allows_only_the_official_captcha_subframe() {
        assert!(BrowserWindowPolicy::allows_navigation(
            &Url::parse("about:blank").unwrap()
        ));
        assert!(BrowserWindowPolicy::allows_navigation(
            &Url::parse("https://rmc.bytedance.com/verifycenter/captcha/v2").unwrap()
        ));
        assert!(!BrowserWindowPolicy::allows_navigation(
            &Url::parse("https://rmc.bytedance.com/unrelated").unwrap()
        ));
        assert!(!BrowserWindowPolicy::allows_navigation(
            &Url::parse("https://rmc.bytedance.com.example.com/verifycenter/captcha/v2").unwrap()
        ));
        assert!(!BrowserWindowPolicy::allows_navigation(
            &Url::parse("http://rmc.bytedance.com/verifycenter/captcha/v2").unwrap()
        ));
        assert!(
            TauriBrowserPageDriver::target_url("https://rmc.bytedance.com/verifycenter/captcha/v2")
                .is_err()
        );
    }

    #[test]
    fn request_tracker_rejects_old_callbacks_after_navigation_or_clear() {
        let tracker = BrowserRequestTracker::default();
        tracker.activate(41);
        assert!(tracker.is_current(41));
        tracker.activate(42);
        assert!(!tracker.is_current(41));
        assert!(tracker.is_current(42));
        tracker.invalidate();
        assert!(!tracker.is_current(42));
    }

    #[test]
    fn snapshot_script_has_bounded_fields_and_no_sensitive_browser_storage_access() {
        assert!(BROWSER_SNAPSHOT_SCRIPT.contains("maxScripts = 32"));
        assert!(BROWSER_SNAPSHOT_SCRIPT.contains("maxBytes = 2097152"));
        assert!(BROWSER_SNAPSHOT_SCRIPT.contains("snapshotOverflow"));
        assert!(BROWSER_SNAPSHOT_SCRIPT.contains("roomOffline"));
        assert!(BROWSER_SNAPSHOT_SCRIPT.contains("challengeVisible"));
        assert!(BROWSER_SNAPSHOT_SCRIPT.contains("checkedTextNodes < 5000"));
        assert!(!BROWSER_SNAPSHOT_SCRIPT.contains("document.cookie"));
        assert!(!BROWSER_SNAPSHOT_SCRIPT.contains("localStorage"));
        assert!(!BROWSER_SNAPSHOT_SCRIPT.contains("outerHTML"));
        assert!(!BROWSER_SNAPSHOT_SCRIPT.contains("__TAURI__"));
        assert!(!BROWSER_SNAPSHOT_SCRIPT.contains("invoke("));
    }

    #[test]
    fn media_muting_script_is_bounded_to_dom_media_elements() {
        assert!(MUTE_MEDIA_SCRIPT.contains("HTMLMediaElement"));
        assert!(MUTE_MEDIA_SCRIPT.contains("node.muted = true"));
        assert!(MUTE_MEDIA_SCRIPT.contains("node.volume = 0"));
        assert!(MUTE_MEDIA_SCRIPT.contains("MutationObserver"));
        assert!(!MUTE_MEDIA_SCRIPT.contains("document.cookie"));
        assert!(!MUTE_MEDIA_SCRIPT.contains("localStorage"));
        assert!(!MUTE_MEDIA_SCRIPT.contains("invoke("));
    }

    #[test]
    fn access_window_is_not_in_the_main_capability() {
        let capability: serde_json::Value =
            serde_json::from_str(include_str!("../capabilities/default.json")).unwrap();
        let windows = capability["windows"].as_array().unwrap();
        assert_eq!(windows, &[serde_json::Value::String("main".to_owned())]);
        assert!(!include_str!("../capabilities/default.json").contains(DOUYIN_ACCESS_WINDOW_LABEL));
    }
}
