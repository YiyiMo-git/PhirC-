prpr_l10n::tl_file!("respack");

use super::{
    library::{request_export, resolve_export, take_export},
    Page, SharedState,
};
use crate::{
    dir, get_data, get_data_mut,
    icons::Icons,
    save_data,
    scene::{compress_folder, confirm_delete, MainScene},
};
use anyhow::{anyhow, Result};
use macroquad::prelude::*;
use prpr::{
    core::{ParticleEmitter, ResPackInfo, ResourcePack},
    ext::{create_audio_manger, poll_future, LocalTask, RectExt, SafeTexture, ScaleType},
    scene::{request_file, show_error, show_message},
    ui::{DRectButton, Dialog, Scroll, Ui},
};
use sasa::{AudioManager, PlaySfxParams, Sfx};
use serde_yaml::Error;
use std::{
    borrow::Cow,
    fs::File,
    io::{BufWriter, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
};

fn build_emitter(pack: &ResourcePack) -> Result<ParticleEmitter> {
    ParticleEmitter::new(pack, get_data().config.note_scale * 0.6, pack.info.hide_particles)
}

/// 导出资源包时的文件名清洗：把路径分隔符等非法字符换成 `_`。
///
/// 资源包名字来自 `info.yml`（用户可控数据），直接拿去当文件名可能被塞进
/// `/` `\` `..` 之类字符从而写到别处，所以导出前必须清洗。
fn sanitize_filename(name: &str) -> String {
    name.chars()
        .map(|c| match c {
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => '_',
            _ => c,
        })
        .collect()
}

pub struct ResPackItem {
    path: Option<PathBuf>,
    name: String,
    btn: DRectButton,
    /// 卡片右上角的删除按钮（内置资源包不显示）。
    delete_btn: DRectButton,

    loaded: Option<ResourcePack>,
    load_task: LocalTask<Result<ResourcePack>>,
}

impl ResPackItem {
    pub fn new(path: Option<PathBuf>, name: String) -> Self {
        Self {
            path,
            name,
            btn: DRectButton::new(),
            delete_btn: DRectButton::new(),

            loaded: None,
            load_task: None,
        }
    }

    fn load(&mut self) {
        if let Some(loaded) = self.loaded.take() {
            self.load_task = Some(Box::pin(async move { Ok(loaded) }));
        } else {
            self.load_task = Some(Box::pin(ResourcePack::from_path(self.path.clone())));
        }
    }
}

pub struct ResPackPage {
    audio: AudioManager,
    items: Vec<ResPackItem>,
    import_btn: DRectButton,
    btns_scroll: Scroll,
    index: usize,

    _icons: Arc<Icons>,

    info_btn: DRectButton,
    /// 导出当前资源包（打包成 zip 让用户另存）。内置包（index 0）没有实体目录，不显示。
    export_btn: DRectButton,
    export_task: Option<mpsc::Receiver<Result<()>>>,
    /// 点导出时记下要打包的目录，等系统另存为对话框返回后按它压缩。
    export_path: Option<PathBuf>,

    should_delete: Arc<AtomicBool>,
    /// 待删除卡片的下标（点某张卡片的删除按钮时先记下，确认后按它删除）。
    delete_idx: usize,

    emitter: Option<ParticleEmitter>,
    sfxs: Option<[Sfx; 3]>,
    /// 预览区「循环下落的音符」轮次。
    ///
    /// 用来保证每轮只发一次命中特效 + 一次音效。
    last_round_preview: u32,
}

impl ResPackPage {
    pub fn new(icons: Arc<Icons>) -> Result<Self> {
        MainScene::take_imported_respack();
        let dir = dir::respacks()?;
        let mut items = vec![ResPackItem::new(None, tl!("default").into_owned())];
        let data = get_data_mut();
        data.respacks = data
            .respacks
            .clone()
            .into_iter()
            .filter(|path| -> bool {
                let p = format!("{dir}/{path}");
                let p = Path::new(&p);
                if !p.is_dir() {
                    return false;
                }
                let cfg = File::open(p.join("info.yml"));
                match cfg {
                    Err(_) => {
                        let _ = std::fs::remove_dir_all(p);
                        false
                    }
                    Ok(cfg) => {
                        let info: Result<ResPackInfo, Error> = serde_yaml::from_reader(cfg);
                        match info {
                            Err(_) => {
                                let _ = std::fs::remove_dir_all(p);
                                false
                            }
                            Ok(info) => {
                                items.push(ResPackItem::new(Some(p.to_owned()), info.name));
                                true
                            }
                        }
                    }
                }
            })
            .collect();

        let respack_id = get_data().respack_id;
        let index = respack_id.min(items.len().saturating_sub(1));
        if index != respack_id {
            data.respack_id = index;
        }

        save_data()?;
        items[index].load();
        Ok(Self {
            audio: create_audio_manger(&get_data().config)?,
            items,
            import_btn: DRectButton::new(),
            btns_scroll: Scroll::new(),
            index,

            _icons: icons,

            info_btn: DRectButton::new(),
            export_btn: DRectButton::new(),
            export_task: None,
            export_path: None,

            should_delete: Arc::new(AtomicBool::default()),
            delete_idx: 0,

            emitter: None,
            sfxs: None,
            last_round_preview: u32::MAX,
        })
    }
}

impl Page for ResPackPage {
    fn label(&self) -> Cow<'static, str> {
        tl!("label")
    }

    fn touch(&mut self, touch: &Touch, s: &mut SharedState) -> Result<bool> {
        let t = s.t;
        if self.btns_scroll.touch(touch, t) {
            return Ok(true);
        }
        if self.import_btn.touch(touch, t) {
            request_file("_import_respack");
            return Ok(true);
        }
        // 每张卡片的删除按钮（先于卡片选择命中，避免点删除反而切到该卡片）
        for (index, item) in self.items.iter_mut().enumerate() {
            if index > 0 && item.delete_btn.touch(touch, t) {
                self.delete_idx = index;
                confirm_delete(self.should_delete.clone());
                return Ok(true);
            }
        }
        if self.items[self.index].load_task.is_none() {
            for (index, item) in self.items.iter_mut().enumerate() {
                if item.btn.touch(touch, t) {
                    self.index = index;
                    get_data_mut().respack_id = index;
                    save_data()?;
                    item.load();
                    return Ok(true);
                }
            }
        }
        if self.info_btn.touch(touch, t) {
            let item = &self.items[self.index];
            let info = &item.loaded.as_ref().unwrap().info;
            Dialog::plain(
                tl!("info"),
                tl!("info-content", "name" => item.name.clone(), "author" => info.author.clone(), "desc" => info.description.clone()),
            )
            .listener(|_dialog, pos| pos == -2)
            .show();
            return Ok(true);
        }
        // 导出当前资源包（内置包没有实体目录，不导出）
        if self.index != 0 && self.export_btn.touch(touch, t) {
            let name = sanitize_filename(&self.items[self.index].name);
            self.export_path = self.items[self.index].path.clone();
            request_export(format!("{name}.zip"));
            return Ok(true);
        }
        Ok(false)
    }

    fn update(&mut self, s: &mut SharedState) -> Result<()> {
        let t = s.t;
        self.btns_scroll.update(t);
        let item = &mut self.items[self.index];
        if let Some(task) = &mut item.load_task {
            if let Some(res) = poll_future(task.as_mut()) {
                match res {
                    Err(err) => {
                        show_error(err.context(tl!("load-failed")));
                    }
                    Ok(val) => {
                        self.emitter = Some(build_emitter(&val)?);
                        self.sfxs = Some([
                            self.audio.create_sfx(val.sfx_click.clone(), None)?,
                            self.audio.create_sfx(val.sfx_drag.clone(), None)?,
                            self.audio.create_sfx(val.sfx_flick.clone(), None)?,
                        ]);
                        item.loaded = Some(val);
                    }
                }
                item.load_task = None;
            }
        }
        if self.should_delete.fetch_and(false, Ordering::Relaxed) {
            let idx = self.delete_idx;
            std::fs::remove_dir_all(self.items[idx].path.as_ref().unwrap())?;
            self.items.remove(idx);
            get_data_mut().respacks.remove(idx - 1);
            if idx < self.index {
                // 删的是当前项之前的卡片：当前项下标前移一位，内容不变
                self.index -= 1;
                get_data_mut().respack_id = self.index;
            } else if idx == self.index {
                // 删的是当前使用的卡片：回退到上一项并重新加载
                self.index = self.index.saturating_sub(1);
                get_data_mut().respack_id = self.index;
                self.items[self.index].load();
            }
            save_data()?;
            show_message(tl!("deleted")).ok();
        }
        if let Some(item) = MainScene::take_imported_respack() {
            self.items.push(item);
        }
        // 导出：系统「另存为」对话框是异步的，这里轮询它的结果。
        // 拿到文件后另起线程压缩目录（IO 重活不能堵渲染线程），失败就删掉半成品文件。
        if let Some(config) = take_export() {
            fn export_inner(path: &Path, output: File) -> Result<()> {
                let mut writer = BufWriter::new(output);
                compress_folder(path, &mut writer)?;
                writer.flush()?;
                Ok(())
            }

            match config {
                Err(err) => show_error(err.into()),
                Ok(config) => {
                    let file = config.file;
                    let deleter = config.deleter;
                    if let Some(path) = self.export_path.take() {
                        let (tx, rx) = mpsc::channel();
                        self.export_task = Some(rx);
                        std::thread::spawn(move || {
                            let result = export_inner(&path, file);
                            if result.is_err() {
                                let _ = (deleter)();
                            }
                            let _ = tx.send(result);
                        });
                    } else {
                        drop(file);
                        let _ = (deleter)();
                        show_error(anyhow!("没有选中的资源包可导出"));
                    }
                }
            }
        }
        if let Some(rx) = &mut self.export_task {
            match rx.try_recv() {
                Ok(Err(err)) => {
                    show_error(err);
                    self.export_task = None;
                }
                Ok(Ok(())) => {
                    resolve_export();
                    self.export_task = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    show_error(anyhow!("导出线程崩溃"));
                    self.export_task = None;
                }
            }
        }
        Ok(())
    }

    fn render(&mut self, ui: &mut Ui, s: &mut SharedState) -> Result<()> {
        let t = s.t;
        let m = crate::newui::Metrics::new(ui);
        let th = crate::newui::theme();

        // 粒子预览的发射器 / 音效先取出来：下面的滚动闭包里要同时用它们和 `self`，
        // 借用会打架。取出来用完再放回去（每帧如此，没有额外开销）。
        let mut emitter = self.emitter.take();
        let mut sfxs = self.sfxs.take();

        // ============================ 版式 ============================
        // 逐字照 phira-0.8.2 `page/respack.rs` 的 `render`：
        //   • 左列表是**固定 0.47 世界单位宽**（不是「内容区宽的 47%」—— 之前正是这么写错的：
        //     内容区 1.88 宽 → 列表被拉到 0.88，预览只剩 0.9 还和列表重叠成一团）；
        //   • 右预览从列表右缘 +0.04 一直吃到内容区右缘，内部几何全用上游的绝对常量
        //     （hold `x = 0.1`、判定线 `cx = 0.73`、音符列 `x = prev_r.x + 0.07`）。
        //
        // 【关键】上游把列表和预览放在**两次独立的 `render_fader`** 里，本分支必须照做：
        // `ui.dx` / `ui.dy` 是**持久**修改变换的（只有 `ui.scope` / `ui.with` 才还原），
        // 合并进同一个闭包时，列表的 `ui.dx(list_r.x)` 会一路泄漏到预览，
        // 把整块预览（面板 + 音符 + hold + 判定线）平移到列表上面 ——
        // 这正是「资源包界面叠成一团」的根因。拆开还顺带恢复了上游的**级联淡入**
        // （列表先入、预览 40ms 后入），页面切换的观感也更顺。
        // ==============================================================
        let content = ui.content_rect();
        // 列表槽位（HUD 编辑器可调），默认就是上游那个「贴左、固定 0.47 宽」的矩形。
        let default_list = Rect::new(-0.92, content.y, 0.47, content.h);
        let list_r = crate::hud::slot_or(ui, "respack", "list", crate::hud::Cap(true, true, true), default_list);
        let prev_r = Rect::new(
            list_r.right() + 0.04,
            list_r.y,
            (content.right() - list_r.right() - 0.04).max(0.2),
            list_r.h,
        );

        let selected = self.index;
        let import_h = m.u(46.);
        let import_r = Rect::new(list_r.x, list_r.y, list_r.w, import_h);
        let row_h = m.u(56.);
        // （原来的 `row_gap = m.u(8.)` 已删：改成 iOS 分组列表后行是**紧贴**的，
        //   分隔靠 `group_row` 画的内缩发丝线，不再需要行间距。）
        let scroll_y = import_r.bottom() + m.u(12.);
        let scroll_h = (list_r.bottom() - scroll_y).max(m.u(10.));
        self.btns_scroll.size((list_r.w, scroll_h));

        // ---------- 1. 左栏：导入 / 导出 + 资源包列表 ----------
        s.render_fader(ui, |ui| {
            self.import_btn.inner.set(ui, import_r);
            crate::newui::acrylic(ui, import_r, m.u(10.), &th);
            if self.import_btn.inner.touching() {
                crate::newui::rounded(ui, import_r, m.u(10.), th.wash(0.06));
            }
            crate::newui::rounded_stroke(ui, import_r, m.u(10.), m.u(1.), th.border);
            crate::newui::draw_icon(ui, &m, crate::newui::Icon::Plus, Rect::new(import_r.x + m.u(14.), import_r.center().y - m.u(8.), m.u(16.), m.u(16.)), th.text, 1.8);
            crate::newui::text(ui, &m, tl!("import"), import_r.x + m.u(38.), import_r.center().y, 15., th.text_hi, (0., 0.5));

            // 导出按钮（内置包 index 0 没有实体目录，不显示）放在导入按钮右边
            if selected != 0 {
                let export_lbl = tl!("export");
                let export_w = crate::newui::measure(ui, &m, export_lbl.clone(), 15.).w + m.u(60.);
                let export_w = export_w.min(import_r.w * 0.48);
                let export_r = Rect::new(import_r.right() - export_w, import_r.y, export_w, import_h);
                self.export_btn.inner.set(ui, export_r);
                crate::newui::acrylic(ui, export_r, m.u(10.), &th);
                if self.export_btn.inner.touching() {
                    crate::newui::rounded(ui, export_r, m.u(10.), th.wash(0.06));
                }
                crate::newui::rounded_stroke(ui, export_r, m.u(10.), m.u(1.), th.border);
                let inner = export_r.feather(-m.u(14.));
                crate::newui::draw_icon(ui, &m, crate::newui::Icon::Download, Rect::new(inner.x, export_r.center().y - m.u(8.), m.u(16.), m.u(16.)), th.text, 1.8);
                crate::newui::text(ui, &m, export_lbl, inner.x + m.u(24.), export_r.center().y, 15., th.text_hi, (0., 0.5));
            }

            // 列表本体。`dx/dy` 必须关在 `scope` 里：它是**持久**变换，
            // 不还原会泄漏到本闭包后面的粒子绘制（以及下一个 `render_fader`）。
            ui.scope(|ui| {
                ui.dx(list_r.x);
                ui.dy(scroll_y);
                self.btns_scroll.render(ui, |ui| {
                    let mut y = 0.;
                    // iOS 分组列表：行与行**紧贴**，分隔靠内缩发丝线
                    // （原来是「每行一张独立圆角卡 + `row_gap`」，那是卡片语言的观感）。
                    // 行比外层 `acrylic` 面板窄，所以 `radius` 传 0 —— 悬停/选中就是方角高亮。
                    let n = self.items.len();
                    for (index, item) in self.items.iter_mut().enumerate() {
                        let row = Rect::new(0., y, list_r.w, row_h);
                        crate::newui::group_row(
                            ui,
                            &th,
                            row,
                            0.,
                            index == 0,
                            index + 1 == n,
                            0.,
                            m.u(14.),
                            if index == selected {
                                th.with_a(th.glow, 0.16)
                            } else {
                                Color::new(0., 0., 0., 0.)
                            },
                        );
                        crate::newui::text_ellipsis(ui, &m, &item.name, row.x + m.u(14.), row.center().y, 14., row.w - m.u(28.), if index == selected { th.text_hi } else { th.text }, (0., 0.5));
                        // 每张卡片的删除按钮（内置资源包不显示）
                        if index > 0 {
                            let dr = Rect::new(row.right() - m.u(34.), row.center().y - m.u(10.), m.u(20.), m.u(20.));
                            item.delete_btn.inner.set(ui, dr);
                            crate::newui::draw_icon(ui, &m, crate::newui::Icon::Delete, dr.feather(-m.u(2.)), th.text_dim, 1.6);
                        }
                        item.btn.inner.set(ui, row);
                        y += row_h;
                    }
                    (list_r.w, y)
                });
            });
        });

        // 【不要给列表行加粒子特效】上游 phira-0.8.2 的列表是纯卡片，没有这个效果。
        // 本分支曾额外在选中行左缘 `emit_at(...)` 打一发「打击特效」，
        // 用户反馈那团特效压在卡片和包名上、看着像多出来的一个东西 —— 已移除。
        // 打击特效只保留右栏预览里判定线上那一次（那才是上游的还原点）。

        // ---------- 2. 右栏：选中资源包预览 ----------
        // 几何逐字照 phira-0.8.2 `page/respack.rs` 的 `render`：
        // 左侧一列两条 hold 长条、右侧一列「音符 + 高亮」三组、下方一条判定线。
        // 原版这一段就是**绝对世界坐标常量**（hold `x = 0.1`、判定线 `cx = r.x + 0.43`），
        // 本分支的预览面板宽度与上游几乎相同，所以直接沿用，不再做百分比混算
        // —— 之前的「变形错位」就是把 `m.u()`（设计稿 px）混进这套世界坐标造成的。
        s.render_fader(ui, |ui| {
            crate::newui::acrylic(ui, prev_r, m.u(12.), &th);
            crate::newui::rounded_stroke(ui, prev_r, m.u(12.), m.u(1.), th.border);

            // 贴图着色的 alpha：**必须是真白**，不能用 `semi_white` ——
            // 那个在浅色主题下返回的是黑墨水（`ui::theme::ink`），会把整个长条乘成黑色。
            // 这是全项目踩过的坑，`page.rs:231` 有同样的注释。
            let tex_tint = Color::new(1., 1., 1., ui.alpha);

            // 信息按钮的位置先定好（右上角，选中且加载完成才画）。
            let info_r = Rect::new(prev_r.right() - m.u(40.), prev_r.y + m.u(12.), m.u(26.), m.u(26.));

            if let Some(pack) = self.items[selected].loaded.as_ref() {
                let width = 0.16;
                let mut r = Rect::new(prev_r.x + 0.07, prev_r.y + 0.1, width, 0.);
                let draw = |ui: &mut Ui, mut r: Rect, tex: Texture2D, mh: Texture2D| {
                    let y = r.y;
                    r.h = tex.height() / tex.width().max(1.) * r.w;
                    r.y = y - r.h / 2.;
                    ui.fill_rect(r, (tex, r, ScaleType::Fit));
                    r.x += r.w * 1.8;
                    r.w *= mh.width() / tex.width().max(1.);
                    r.x -= r.w / 2.;
                    r.h = mh.height() / mh.width().max(1.) * r.w;
                    r.y = y - r.h / 2.;
                    ui.fill_rect(r, (mh, r, ScaleType::Fit));
                };
                let sp = (prev_r.h - 0.4) / 2.;
                draw(ui, r, *pack.note_style.click, *pack.note_style_mh.click);
                r.y += sp;
                draw(ui, r, *pack.note_style.drag, *pack.note_style_mh.drag);
                r.y += sp;
                draw(ui, r, *pack.note_style.flick, *pack.note_style_mh.flick);

                // ---------- hold 长条（上游绝对 `x = 0.1`，两条并排） ----------
                let mut hr = Rect::new(0.1, prev_r.y + 0.1, width, prev_r.h - 0.38);
                let conv = |r: Rect, tex: &SafeTexture| Rect::new(r.x * tex.width(), r.y * tex.height(), r.w * tex.width(), r.h * tex.height());
                let hold = |ui: &mut Ui, mut r: Rect, style: &prpr::core::NoteStyle, width: f32| {
                    let factor = if pack.info.hold_compact { 0.5 } else { 1. };
                    // 尾
                    let tr = conv(style.hold_tail_rect(), &style.hold);
                    let h = tr.h / tr.w.max(1.) * width;
                    let r2 = ui.rect_to_global(Rect::new(r.x, r.y - h * factor, width, h));
                    draw_texture_ex(
                        *style.hold,
                        r2.x,
                        r2.y,
                        tex_tint,
                        DrawTextureParams { source: Some(tr), dest_size: Some(vec2(r2.w, r2.h)), ..Default::default() },
                    );
                    // 头
                    let tr = conv(style.hold_head_rect(), &style.hold);
                    let h = tr.h / tr.w.max(1.) * width;
                    let r2 = ui.rect_to_global(Rect::new(r.x, r.bottom() - h * (1. - factor), width, h));
                    draw_texture_ex(
                        *style.hold,
                        r2.x,
                        r2.y,
                        tex_tint,
                        DrawTextureParams { source: Some(tr), dest_size: Some(vec2(r2.w, r2.h)), ..Default::default() },
                    );
                    // 体
                    r.w = width;
                    let r2 = ui.rect_to_global(r);
                    draw_texture_ex(
                        if pack.info.hold_repeat {
                            **style.hold_body.as_ref().unwrap()
                        } else {
                            *style.hold
                        },
                        r2.x,
                        r2.y,
                        tex_tint,
                        DrawTextureParams {
                            source: Some(if pack.info.hold_repeat {
                                let body = style.hold_body.as_ref().unwrap();
                                let bw = body.width();
                                Rect::new(0., 0., bw, r2.h / width.max(1.) / 2. * bw)
                            } else {
                                conv(style.hold_body_rect(), &style.hold)
                            }),
                            dest_size: Some(vec2(r2.w, r2.h)),
                            ..Default::default()
                        },
                    );
                };
                hold(ui, hr, &pack.note_style, width);
                hr.x += width + 0.04;
                hold(
                    ui,
                    hr,
                    &pack.note_style_mh,
                    width * pack.note_style_mh.hold.width() / pack.note_style.hold.width().max(1.),
                );

                // ---------- 判定线 + 循环下落的音符（上游 `line = 0.12`、`cx = r.x + 0.43`） ----------
                let inter = 1.5;
                let rnd = t.div_euclid(inter);
                let irnd = rnd as u32;
                let tex = match irnd % 3 {
                    0 => *pack.note_style.click,
                    1 => *pack.note_style.drag,
                    2 => *pack.note_style.flick,
                    _ => unreachable!(),
                };
                // hold 画完后 `hr.x` = 0.1 + 0.16 + 0.04 = 0.30，所以判定线中心在 0.73，
                // 线宽 0.4 → 覆盖 [0.53, 0.93]，正好落在预览面板 [−0.41, 0.94] 内。
                let st = hr.y + 0.06;
                let cx = hr.x + 0.43;
                let line = 0.12;
                ui.fill_rect(Rect::new(cx - 0.2, line - 0.004, 0.4, 0.008), WHITE);
                let p = (t - inter * rnd) / 0.9;
                if p <= 1. {
                    let y = st + (line - st) * p;
                    let h = tex.height() / tex.width().max(1.) * width;
                    let nr = Rect::new(cx - width / 2., y - h / 2., width, h);
                    ui.fill_rect(nr, (tex, nr, ScaleType::Fit));
                } else if irnd != self.last_round_preview {
                    // 预览的循环轮次与列表粒子的轮次**分开记录**：两者共用同一个时钟，
                    // 共用一份记录时列表会先把它置掉，这条分支永远进不来（预览命中特效一直是死的）。
                    self.last_round_preview = irnd;
                    if let Some(em) = &mut emitter {
                        em.emit_at(vec2(cx, line), 0., pack.info.fx_perfect());
                    }
                    if let Some(sfxs) = &mut sfxs {
                        let _ = sfxs[(irnd % 3) as usize].play(PlaySfxParams {
                            amplifier: get_data().config.volume_sfx,
                        });
                    }
                }
            } else {
                let c = prev_r.center();
                ui.loading(c.x, c.y, t, WHITE, ());
            }

            // 粒子统一在这一层画一次（绝对坐标，盖在预览面板之上）。
            // 原来这里读的是 `self.emitter`，而它在函数开头已被 `take()` 走 —— 恒为 None，
            // 预览的命中特效因此从来没画出来过。
            if let Some(em) = &mut emitter {
                em.draw(get_frame_time());
            }

            // 包名（底部，永远贴着面板下缘）
            if self.items[selected].loaded.is_some() {
                crate::newui::text_ellipsis(
                    ui,
                    &m,
                    &self.items[selected].name,
                    prev_r.x + m.u(20.),
                    prev_r.bottom() - m.u(24.),
                    15.,
                    prev_r.w - m.u(72.),
                    th.text_hi,
                    (0., 0.5),
                );
            }

            // 信息按钮（右上角，选中且加载完成）
            if self.items[selected].loaded.is_some() {
                self.info_btn.inner.set(ui, info_r);
                crate::newui::rounded(ui, info_r, m.u(8.), th.wash(0.06));
                crate::newui::draw_icon(ui, &m, crate::newui::Icon::Info, info_r.feather(-m.u(6.)), th.text_dim, 1.6);
            }
        });

        // 放回预览用的发射器 / 音效，供下一帧继续
        self.emitter = emitter;
        self.sfxs = sfxs;
        Ok(())
    }
}
