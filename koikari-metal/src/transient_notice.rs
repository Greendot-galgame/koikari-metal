//! Time-bounded, non-fatal notices, independent of the renderer and audio device.
//!
//! Repeating the same notice must not extend its deadline or revive it after
//! expiry. Clear the context on the next dialogue stop to allow a fresh notice.
use std::time::{Duration, Instant};

pub(crate) const AUDIO_NOTICE_LIFETIME: Duration = Duration::from_secs(4);
pub(crate) const SUCCESS_NOTICE_LIFETIME: Duration = Duration::from_millis(2500);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum NoticeKind {
    Error,
    Warning,
    Success,
}

impl NoticeKind {
    pub(crate) fn background(self) -> [f32; 4] {
        match self {
            Self::Error => [0.35, 0.05, 0.05, 0.86],
            Self::Warning => [0.24, 0.16, 0.03, 0.78],
            Self::Success => [0.035, 0.22, 0.19, 0.82],
        }
    }
}

#[derive(Default)]
pub(crate) struct TransientNotice {
    entry: Option<Notice>,
}

struct Notice {
    message: String,
    expires_at: Instant,
}

impl TransientNotice {
    /// Return true for a new notice (also useful for deduplicating diagnostics).
    pub(crate) fn show(&mut self, message: String, now: Instant) -> bool {
        if self
            .entry
            .as_ref()
            .is_some_and(|entry| entry.message == message)
        {
            return false;
        }
        self.entry = Some(Notice {
            message,
            expires_at: now + AUDIO_NOTICE_LIFETIME,
        });
        true
    }

    /// An explicit completed action (F5/F9) starts a new 2.5-second notice,
    /// including when the same action is repeated. Background warnings retain
    /// show()'s separate no-extension/no-revival policy.
    pub(crate) fn show_success(&mut self, message: String, now: Instant) {
        self.entry = Some(Notice {
            message,
            expires_at: now + SUCCESS_NOTICE_LIFETIME,
        });
    }

    pub(crate) fn clear(&mut self) {
        self.entry = None;
    }

    pub(crate) fn active_text(&self, now: Instant) -> Option<&str> {
        self.entry
            .as_ref()
            .filter(|entry| now < entry.expires_at)
            .map(|entry| entry.message.as_str())
    }

    /// Persistent errors always win. Their presence must not pause the notice's
    /// independent expiry clock or erase the persistent error on expiry.
    pub(crate) fn display<'a>(
        &'a self,
        persistent: Option<&'a str>,
        now: Instant,
    ) -> Option<&'a str> {
        persistent.or_else(|| self.active_text(now))
    }
}

/// Success feedback never conceals a real error or an active audio warning.
pub(crate) fn visible_notice<'a>(
    persistent: Option<&'a str>,
    audio: &'a TransientNotice,
    success: &'a TransientNotice,
    now: Instant,
) -> Option<(NoticeKind, &'a str)> {
    if let Some(message) = audio.display(persistent, now) {
        let kind = if persistent.is_some() {
            NoticeKind::Error
        } else {
            NoticeKind::Warning
        };
        Some((kind, message))
    } else {
        success
            .active_text(now)
            .map(|message| (NoticeKind::Success, message))
    }
}

/// Keep the in-game notice short; the diagnostic log retains the full message
/// and complete error chain. Use Unicode characters, never slice a UTF-8 byte.
pub(crate) fn audio_summary(diagnostic: &str) -> String {
    let mut chars = diagnostic
        .lines()
        .next()
        .unwrap_or(diagnostic)
        .trim()
        .chars();
    let short: String = chars.by_ref().take(72).collect();
    let suffix = if chars.next().is_some() { "…" } else { "" };
    format!("音频提示：{short}{suffix}（详见终端日志）")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_and_load_success_expire_after_two_and_a_half_seconds() {
        let now = Instant::now();
        for message in ["已快速存档 (F9 读取)", "已读取快速存档"] {
            let mut notice = TransientNotice::default();
            notice.show_success(message.into(), now);
            assert_eq!(notice.active_text(now), Some(message));
            assert!(
                notice
                    .active_text(now + Duration::from_millis(2499))
                    .is_some()
            );
            assert!(
                notice
                    .active_text(now + Duration::from_millis(2500))
                    .is_none()
            );
            assert!(notice.active_text(now + Duration::from_secs(30)).is_none());
        }
    }

    #[test]
    fn repeating_save_or_load_refreshes_the_success_deadline() {
        let now = Instant::now();
        let mut notice = TransientNotice::default();
        notice.show_success("saved".into(), now);
        notice.show_success("saved".into(), now + Duration::from_secs(2));
        assert_eq!(
            notice.active_text(now + Duration::from_secs(3)),
            Some("saved")
        );
        assert!(
            notice
                .active_text(now + Duration::from_millis(4500))
                .is_none()
        );
        notice.show_success("loaded".into(), now + Duration::from_secs(6));
        assert_eq!(
            notice.active_text(now + Duration::from_secs(7)),
            Some("loaded")
        );
    }

    #[test]
    fn success_feedback_is_not_red_and_cannot_cover_real_errors() {
        let now = Instant::now();
        let mut audio = TransientNotice::default();
        let mut success = TransientNotice::default();
        success.show_success("saved".into(), now);
        assert_eq!(
            visible_notice(None, &audio, &success, now),
            Some((NoticeKind::Success, "saved"))
        );
        let color = NoticeKind::Success.background();
        assert!(color[1] > color[0] && color[2] > color[0]);
        assert_eq!(
            visible_notice(Some("save failed"), &audio, &success, now),
            Some((NoticeKind::Error, "save failed"))
        );
        audio.show("audio failed".into(), now);
        assert_eq!(
            visible_notice(None, &audio, &success, now),
            Some((NoticeKind::Warning, "audio failed"))
        );
        assert_eq!(
            visible_notice(
                Some("script failed"),
                &audio,
                &success,
                now + Duration::from_secs(10)
            ),
            Some((NoticeKind::Error, "script failed"))
        );
        assert!(visible_notice(None, &audio, &success, now + Duration::from_secs(10)).is_none());
    }

    #[test]
    fn notice_expires_at_four_seconds_without_sleep() {
        let now = Instant::now();
        let mut notice = TransientNotice::default();
        assert!(notice.show("no audio device".into(), now));
        assert_eq!(notice.active_text(now), Some("no audio device"));
        assert!(
            notice
                .active_text(now + Duration::from_millis(3999))
                .is_some()
        );
        assert!(notice.active_text(now + Duration::from_secs(4)).is_none());
        assert!(notice.active_text(now + Duration::from_secs(400)).is_none());
    }

    #[test]
    fn repeated_background_warning_cannot_extend_or_revive_notice() {
        let now = Instant::now();
        let mut notice = TransientNotice::default();
        notice.show("restart failed".into(), now);
        assert!(!notice.show("restart failed".into(), now + Duration::from_secs(3)));
        assert!(notice.active_text(now + Duration::from_secs(4)).is_none());
        assert!(!notice.show("restart failed".into(), now + Duration::from_secs(5)));
        assert!(notice.active_text(now + Duration::from_secs(5)).is_none());
    }

    #[test]
    fn next_dialogue_clears_notice_and_starts_a_new_context() {
        let now = Instant::now();
        let mut notice = TransientNotice::default();
        notice.show("decode error".into(), now);
        notice.clear();
        assert!(notice.active_text(now + Duration::from_secs(1)).is_none());
        assert!(notice.show("decode error".into(), now + Duration::from_secs(1)));
        assert!(notice.active_text(now + Duration::from_secs(4)).is_some());
        assert!(notice.active_text(now + Duration::from_secs(5)).is_none());
    }

    #[test]
    fn persistent_script_or_layer_error_wins_and_is_never_timed_out() {
        let now = Instant::now();
        let mut notice = TransientNotice::default();
        notice.show("voice decoder failed".into(), now);
        let persistent = "cannot upload background";
        assert_eq!(notice.display(Some(persistent), now), Some(persistent));
        assert_eq!(
            notice.display(Some(persistent), now + Duration::from_secs(5)),
            Some(persistent)
        );
        assert!(notice.display(None, now + Duration::from_secs(5)).is_none());
        notice.clear();
        assert_eq!(notice.display(Some(persistent), now), Some(persistent));
    }

    #[test]
    fn distinct_error_replaces_old_notice_with_its_own_deadline() {
        let now = Instant::now();
        let mut notice = TransientNotice::default();
        notice.show("voice failed".into(), now);
        assert!(notice.show("music failed".into(), now + Duration::from_secs(3)));
        assert_eq!(
            notice.active_text(now + Duration::from_secs(6)),
            Some("music failed")
        );
        assert!(notice.active_text(now + Duration::from_secs(7)).is_none());
    }

    #[test]
    fn summary_is_unicode_safe_short_and_does_not_mutate_full_diagnostic() {
        let diagnostic = format!(
            "load voice: {}\ncaused by malformed packet",
            "错误".repeat(80)
        );
        let summary = audio_summary(&diagnostic);
        assert!(summary.contains('…'));
        assert!(!summary.contains("malformed packet"));
        assert!(summary.chars().count() < 100);
        assert!(diagnostic.contains("malformed packet"));
    }
}
