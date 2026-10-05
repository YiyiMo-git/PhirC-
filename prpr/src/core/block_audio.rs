//! Native JudgeControl/ProgressControl gesture transitions (4.0.1).
//! 官方 LevelControl 预设为 1500 Hz / 0.1 s。
//!
//! 适配说明：本分支依赖的 sasa 只有单极点 `Music::set_low_pass(系数)`
//! （`0` = 直通，越大越闷；见 `sasa/src/renderer/music.rs`），没有官方的
//! `try_set_low_pass_filter(cutoff, resonance, seconds)` 时分扫掠。这里用
//! 一个等效系数近似官方 1500 Hz 的听感，触摸进 / 出区仍是「边沿触发」。

/// 官方把入区触摸的 BGM 压到 1500 Hz 低通；换算成本分支 sasa 的系数。
const BLOCK_LOW_PASS: f32 = 0.9;

#[derive(Default)]
pub(crate) struct BlockAudio {
    enabled: Option<bool>,
    used: bool,
}

impl BlockAudio {
    pub(crate) fn reset(&mut self) {
        *self = Self::default();
    }

    pub(crate) fn sync(&mut self, music: &mut sasa::Music, touching: bool) {
        self.sync_with(touching, |low_pass| music.set_low_pass(low_pass).is_ok());
    }

    pub(crate) fn suspend(&mut self, music: &mut sasa::Music) {
        if self.used {
            self.reset();
        }
        self.sync(music, false);
    }

    fn sync_with(&mut self, touching: bool, send: impl FnOnce(f32) -> bool) {
        if self.enabled == Some(touching) {
            return;
        }
        let coeff = if touching { BLOCK_LOW_PASS } else { 0. };
        if send(coeff) {
            self.enabled = Some(touching);
            self.used |= touching;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_entry_release_reentry_and_multi_touch_use_native_transitions() {
        let mut control = BlockAudio::default();
        let mut commands = Vec::new();
        for touching in [false, true, true, true, false, false, true] {
            control.sync_with(touching, |f| {
                commands.push(f);
                true
            });
        }
        // 边沿触发：仅在 false→true / true→false 时各发一次命令。
        assert_eq!(commands, [0., BLOCK_LOW_PASS, 0., BLOCK_LOW_PASS]);
        control.reset();
        control.sync_with(false, |f| {
            assert_eq!(f, 0.);
            true
        });
    }

    #[test]
    fn full_queue_retries_without_marking_a_transition_complete() {
        let mut control = BlockAudio::default();
        control.sync_with(true, |_| false);
        assert_eq!(control.enabled, None);
        assert!(!control.used);
        control.sync_with(true, |f| {
            assert_eq!(f, BLOCK_LOW_PASS);
            true
        });
        control.sync_with(false, |_| false);
        control.sync_with(false, |f| {
            assert_eq!(f, 0.);
            true
        });
    }
}
