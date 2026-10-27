//! 回放**播放驱动**：把磁带喂回游戏。
//!
//! 集成方式（对引擎零侵入）：
//! * 启动回放 = 用 `GameMode::Replay` 正常开谱，但挂一个 `update_fn`（本模块造）；
//! * `update_fn` 每帧被 `GameScene::update` 调用（在 `judge.update` 之后），
//!   它按当前谱面时间把磁带触摸 `set_playback_touches()` 塞进去；
//! * 下一帧 `Judge::on_new_frame` 优先用注入的触摸、跳过 macroquad 采集，
//!   flick 追踪 / 判定 / 计分整条管线原样跑起来 —— 观感与当年录制时一致。
//!
//! 一帧延迟说明：注入发生在第 N 帧的 `update_fn`，被第 N+1 帧的 `on_new_frame`
//! 消费。驱动按「下一帧的时间窗」取触摸，正好对齐。
//!
//! 暂停处理：谱面时间不前进说明游戏暂停了，这时注入空触摸，
//! 恢复后不会把上一时刻的 `Started` 重复打进去。

use prpr::{
    judge::Judge,
    replay::{Replay, TouchPlayer},
    scene::UpdateFn,
};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};

/// 槽 A：桥命令放入，`take_next_scene` 取走建 SongScene（信息页）。
static PENDING_SCENE: Mutex<Option<Replay>> = Mutex::new(None);
/// 槽 B：SongScene 按 play 时放入，`global_launch` 取走开局。
static PENDING_LAUNCH: Mutex<Option<Replay>> = Mutex::new(None);

/// 是否正在播放回放（`global_launch` 据此关掉上传，`on_result` 据此跳过写历史）。
static PLAYING: AtomicBool = AtomicBool::new(false);

pub fn is_playing() -> bool {
    PLAYING.load(Ordering::Relaxed)
}

/// 排队一个回放：先建场景（信息页），按 play 才真正播放。
pub fn queue(replay: Replay) {
    *PENDING_SCENE.lock().unwrap() = Some(replay);
}

/// 槽 A：取走要建场景的回放（`take_next_scene` 用）。
pub fn take_pending_scene() -> Option<Replay> {
    PENDING_SCENE.lock().unwrap().take()
}

/// 槽 B：用户在信息页按了 play —— 把磁带转交开局流程。
pub fn queue_for_launch(replay: Replay) {
    *PENDING_LAUNCH.lock().unwrap() = Some(replay);
}

/// 槽 B：取走待开局回放；同时把「回放模式」打开。
pub fn take_pending() -> Option<Replay> {
    let r = PENDING_LAUNCH.lock().unwrap().take();
    PLAYING.store(r.is_some(), Ordering::Relaxed);
    r
}

/// 回放结束（自然播完 / 用户退出）。清掉注入的触摸，恢复真实输入。
pub fn finish_playing() {
    PLAYING.store(false, Ordering::Relaxed);
    prpr::replay::clear_playback_touches();
}

/// 造 `update_fn`：每帧推进磁带并注入触摸。
///
/// * `speed`：播放倍速（0.25x ~ 2x，磁带里录的是什么就按什么）。
/// * `from`：从磁带第几秒开始（拖动定位用；重头播传 0）。
pub fn build_driver(replay: Replay, speed: f32, from: f64) -> UpdateFn {
    let mut player = TouchPlayer::new();
    let mut last_inject_t = from;
    let mut first = true;
    Box::new(move |t, _res, _judge: &mut Judge| {
        if first {
            // 首帧套回录制时的玩法参数与偏移，保证观感一致
            replay.gameplay.apply(&mut _res.config);
            _res.config.offset = replay.offset;
            first = false;
        }
        // 下一帧大约到 t + dt；取这个时间窗的触摸喂进去
        let window = t + (1. / 60.) * speed as f64;
        if (window - last_inject_t).abs() < 1e-6 {
            // 时间没动（暂停中）→ 注入空，避免重复 Started
            prpr::replay::set_playback_touches(Vec::new());
            return;
        }
        last_inject_t = window;
        let touches = player.advance(&replay, window.max(from));
        prpr::replay::set_playback_touches(touches);
    })
}
