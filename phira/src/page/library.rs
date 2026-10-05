prpr_l10n::tl_file!("library");

use super::{CollectionPage, FavoritesPage, NextPage, Page, SharedState};
use crate::{
    charts_view::{ChartDisplayItem, ChartsView, NEED_UPDATE},
    client::{recv_raw, Chart, ChartRef, ChartRefChartInfo, Client, Collection, CollectionUpdate, LocalCollection},
    dir, get_data, get_data_mut,
    icons::Icons,
    newui::{self, Icon, Metrics},
    page::{favorites::FAV_PAGE_RESULT, ChartItem, ChartType, Illustration, SongDetailPage},
    popup::Popup,
    rate::RateDialog,
    save_data,
    scene::{check_read_tos_and_policy, compress_folder, confirm_dialog, ChartOrder, JUST_LOADED_TOS},
    tabs::{Tabs, TitleFn},
    tags::TagsDialog,
};
use anyhow::{anyhow, Error, Result};
use chrono::{DateTime, Utc};
use inputbox::InputBox;
#[cfg(target_os = "android")]
use jni::{jni_sig, jni_str, objects::JObject, refs::Global, vm::JavaVM, EnvUnowned};
use macroquad::prelude::*;
use prpr::{
    ext::{poll_future, JoinToString, LocalTask, RectExt, SafeTexture},
    scene::{request_file, request_input, return_input, show_error, show_message, take_input, NextScene},
    task::Task,
    ui::{button_hit, DRectButton, Dialog, RectButton, Ui},
};
use serde::{Deserialize, Serialize};
use std::{
    any::Any,
    borrow::Cow,
    cell::RefCell,
    collections::{HashMap, HashSet},
    fs::File,
    io::{self, BufWriter, Cursor, Write},
    mem,
    path::Path,
    sync::{
        atomic::{AtomicBool, AtomicU32, Ordering},
        mpsc, Arc, Mutex,
    },
};
use tap::Tap;
use uuid::Uuid;

pub static FAV_UPDATED: AtomicBool = AtomicBool::new(false);
pub static CHOOSE_COVER: AtomicBool = AtomicBool::new(false);

thread_local! {
    pub static CHOSEN_COVER: RefCell<Option<Result<i32, String>>> = const { RefCell::new(None) };
}

const PAGE_NUM: u64 = 28;

#[derive(Clone, Copy, PartialEq, Eq)]
enum ChartListType {
    Local,
    Ranked,
    Special,
    Unstable,
    Popular,
}

struct ChartList {
    ty: ChartListType,
    view: ChartsView,
}
impl ChartList {
    fn new(ty: ChartListType, icons: Arc<Icons>, rank_icons: [SafeTexture; 8]) -> Self {
        let mut view = ChartsView::new(icons, rank_icons);
        view.can_refresh = ty != ChartListType::Local;
        // New UI：点卡片进详情页而不是直接开打
        view.open_detail = true;
        Self { ty, view }
    }
}

struct CreateFavorite {
    name: String,
    charts: Vec<ChartRef>,
}

struct ManageFavorite {
    uuid: Uuid,
    charts: Vec<ChartRef>,
    add: bool,
}

type OnlineTaskResult = (Vec<ChartDisplayItem>, Vec<Chart>, u64);
type OnlineTask = Task<Result<OnlineTaskResult>>;

pub struct LibraryPage {
    tabs: Tabs<ChartList>,

    current_page: u64,
    online_total_page: u64,
    prev_page_btn: DRectButton,
    next_page_btn: DRectButton,

    online_task: Option<OnlineTask>,

    icons: Arc<Icons>,
    rank_icons: [SafeTexture; 8],

    import_btn: DRectButton,

    search_btn: DRectButton,
    search_str: String,
    search_clr_btn: RectButton,

    /// New UI 头部排序组（默认 / 曲名 / 难度 / 评分）+ 正倒序切换。
    sort_chips: [DRectButton; 4],
    sort_rev_btn: DRectButton,

    /// 页面入场时刻（头部/网格的入场动画）。
    enter_t: f32,

    order_btn: DRectButton,
    order_menu: Popup,
    order_menu_options: Vec<ChartOrder>,
    need_show_order_menu: bool,
    current_order: ChartOrder,
    order_meta_menu: Popup,
    need_show_order_meta_menu: bool,

    order_rev: bool,

    filter_btn: DRectButton,
    tags: TagsDialog,
    tags_last_show: bool,
    rating: RateDialog,
    rating_last_show: bool,
    filter_show_tag: bool,

    // 收藏夹 || Favorites
    fav_btn: DRectButton,
    // None = 显示全部 || show all,      Some(folder_name) = 过滤指定收藏夹 || filter by folder
    current_fav_index: Option<usize>,
    sync_fav_task: Option<Task<Result<Option<Collection>>>>,
    force_sync_to_cloud: Arc<AtomicBool>,

    multi_operation_btn: DRectButton,
    multi_operation_menu: Popup,
    multi_operation_options: Vec<&'static str>,
    need_show_multi_operation_menu: bool,

    manage_fav_menu: Popup,
    manage_fav_menu_options: Vec<(Uuid, bool)>,
    need_show_manage_fav_menu: bool,
    manage_fav_task: Option<Task<Result<(Collection, bool)>>>,
    manage_fav_pre_task: Option<Task<Result<ManageFavorite>>>,
    #[allow(clippy::type_complexity)]
    refresh_local_fav_task: Option<Task<Result<(Uuid, Vec<ChartRef>)>>>,

    multi_select_btn: DRectButton,
    multi_select_menu: Popup,
    need_show_multi_select_menu: bool,

    multi_select_cancel_btn: DRectButton,
    delete_multi: Arc<AtomicBool>,
    multi_create_fav_task: Option<Task<Result<CreateFavorite>>>,

    next_page: Option<NextPage>,
    next_page_task: LocalTask<Result<NextPage>>,

    /// 课题模式（T7.3）的选择面板 + 待进入的课题场景。
    challenge: crate::page::ChallengePanel,
    challenge_scene: Option<NextScene>,
    /// 自定义 RKS（T7.4）的编辑面板。
    rks: crate::page::RksPanel,
    /// 最佳谱面榜（T7.2）及其长图导出。
    best_board: crate::page::BestBoard,
    best_board_export: crate::page::BestBoardExport,

    export_paths: Option<Vec<String>>,
    export_task: Option<mpsc::Receiver<Result<()>>>,
    export_progress: Arc<AtomicU32>,
    export_total: usize,
}

impl LibraryPage {
    pub fn new(icons: Arc<Icons>, rank_icons: [SafeTexture; 8]) -> Result<Self> {
        NEED_UPDATE.store(true, Ordering::Relaxed);
        let icon_star = icons.star.clone();
        let new_list = |ty| ChartList::new(ty, Arc::clone(&icons), rank_icons.clone());
        Ok(Self {
            tabs: Tabs::new([
                (new_list(ChartListType::Local), || tl!("local")),
                (new_list(ChartListType::Ranked), || ttl!("chart-ranked")),
                (new_list(ChartListType::Special), || ttl!("chart-special")),
                (new_list(ChartListType::Unstable), || ttl!("chart-unstable")),
                (new_list(ChartListType::Popular), || tl!("popular")),
            ] as [(ChartList, TitleFn); 5]),

            current_page: 0,
            online_total_page: 0,
            prev_page_btn: DRectButton::new(),
            next_page_btn: DRectButton::new(),

            online_task: None,

            icons,
            rank_icons,

            import_btn: DRectButton::new(),

            search_btn: DRectButton::new(),
            search_str: String::new(),
            search_clr_btn: RectButton::new(),

            sort_chips: [DRectButton::new(), DRectButton::new(), DRectButton::new(), DRectButton::new()],
            sort_rev_btn: DRectButton::new().no_sound(),

            enter_t: f32::NEG_INFINITY,

            order_btn: DRectButton::new(),
            order_menu: Popup::new().with_size(0.5),
            order_menu_options: Vec::new(),
            need_show_order_menu: false,
            current_order: ChartOrder::Default,
            order_meta_menu: Popup::new().with_size(0.5),
            need_show_order_meta_menu: false,

            order_rev: true,

            filter_btn: DRectButton::new(),
            tags: TagsDialog::new(true).tap_mut(|it| it.perms = get_data().me.as_ref().map(|it| it.perms()).unwrap_or_default()),
            tags_last_show: false,
            rating: RateDialog::new(icon_star, true).tap_mut(|it| {
                it.rate.score = 3;
                it.rate_upper.as_mut().unwrap().score = 10;
            }),
            rating_last_show: false,
            filter_show_tag: true,

            fav_btn: DRectButton::new(),
            current_fav_index: None,
            sync_fav_task: None,
            force_sync_to_cloud: Arc::default(),

            multi_operation_btn: DRectButton::new(),
            multi_operation_menu: Popup::new().with_size(0.5),
            multi_operation_options: Vec::new(),
            need_show_multi_operation_menu: false,

            manage_fav_menu: Popup::new().with_size(0.5).tap_mut(|it| it.set_auto_dismiss(false)),
            manage_fav_menu_options: Vec::new(),
            need_show_manage_fav_menu: false,
            manage_fav_task: None,
            manage_fav_pre_task: None,
            refresh_local_fav_task: None,

            multi_select_btn: DRectButton::new(),
            multi_select_menu: Popup::new()
                .with_size(0.5)
                .with_options(vec![tl!("multi-select-all").into_owned(), tl!("multi-select-invert").into_owned()]),
            need_show_multi_select_menu: false,

            multi_select_cancel_btn: DRectButton::new(),
            delete_multi: Arc::default(),
            multi_create_fav_task: None,

            next_page: None,
            next_page_task: None,

            challenge: crate::page::ChallengePanel::new(),
            challenge_scene: None,

            rks: crate::page::RksPanel::new()?,
            best_board: crate::page::BestBoard::new(),
            best_board_export: crate::page::BestBoardExport::default(),

            export_paths: None,
            export_task: None,
            export_progress: Arc::default(),
            export_total: 0,
        })
    }
}

impl LibraryPage {
    fn total_page(&self) -> u64 {
        if self.tabs.selected().ty == ChartListType::Local {
            0
        } else {
            self.online_total_page
        }
    }

    pub fn load_online(&mut self) {
        if get_data().config.offline_mode {
            show_message(tl!("offline-mode")).error();
            return;
        }
        if get_data().me.is_none() {
            show_error(anyhow!(tl!("must-login")));
            return;
        }
        if !check_read_tos_and_policy(false, false) {
            return;
        }
        self.tabs.selected_mut().view.reset_scroll();
        self.tabs.selected_mut().view.clear();
        let page = self.current_page;
        let search = self.search_str.clone();
        let order = {
            let order = match self.current_order {
                ChartOrder::Default => "updated",
                ChartOrder::Name => "name",
                ChartOrder::Rating => "rating",
                ChartOrder::Difficulty => "difficulty",
            };
            if self.order_rev {
                format!("-{order}")
            } else {
                order.to_owned()
            }
        };
        let tags = self
            .tags
            .tags
            .tags()
            .iter()
            .cloned()
            .chain(self.tags.unwanted.as_ref().unwrap().tags().iter().map(|it| format!("-{it}")))
            .join(",");
        let division = self.tags.division;
        let rating_range = format!("{},{}", self.rating.rate.score as f32 / 10., self.rating.rate_upper.as_ref().unwrap().score as f32 / 10.);
        let chosen = self.tabs.selected().ty;
        let popular = chosen == ChartListType::Popular;
        let typ = match chosen {
            ChartListType::Ranked => 0,
            ChartListType::Special => 1,
            ChartListType::Unstable => 2,
            _ => -1,
        };
        let by_me = if self.tags.show_me {
            get_data().me.as_ref().map(|it| it.id)
        } else {
            None
        };
        let show_unreviewed = self.tags.show_unreviewed;
        let show_stabilize = self.tags.show_stabilize;
        self.online_task = Some(Task::new(async move {
            let mut q = Client::query::<Chart>();
            if popular {
                q = q.suffix("/popular");
            } else {
                q = q.search(search).order(order).tags(tags).query("rating", rating_range);
            }
            if let Some(me) = by_me {
                q = q.query("uploader", me.to_string());
            }
            if show_stabilize {
                q = q.query("stableRequest", "true");
            } else if show_unreviewed {
                q = q.query("reviewed", "false").query("stableRequest", "false");
            }
            let (remote_charts, count) = q
                .query("type", typ.to_string())
                .query("division", division)
                .page(page)
                .page_num(PAGE_NUM)
                .send()
                .await?;
            let total_page = if count == 0 { 0 } else { (count - 1) / PAGE_NUM + 1 };
            let charts: Vec<_> = remote_charts.iter().map(ChartDisplayItem::from_remote).collect();
            Ok((charts, remote_charts, total_page))
        }));
    }

    fn sync_local(&mut self, s: &SharedState) {
        // 自定义 RKS（T7.4）：曲库变动时同步重算 RKS 汇总（入口按钮和 Bn 都要用）
        self.rks.refresh(&s.charts_local);
        let mut charts_local = s.charts_local.iter().collect::<Vec<_>>();
        self.current_order.apply(&mut charts_local, |it| it);
        if self.order_rev {
            charts_local.reverse();
        }

        let search_by_id = if let Some(id_str) = self.search_str.strip_prefix('#') {
            id_str.trim().parse::<i32>().ok()
        } else {
            None
        };
        let local_matcher = |chart: &ChartItem| {
            if let Some(search_id) = search_by_id {
                chart.info.id == Some(search_id)
            } else {
                chart.info.name.to_ascii_lowercase().contains(&self.search_str.to_ascii_lowercase())
            }
        };

        let list = self.tabs.selected_mut();
        if list.ty == ChartListType::Local {
            let mut charts = Vec::new();
            if let Some(fav_index) = self.current_fav_index {
                let local_chart_map: HashMap<&str, &ChartItem> = charts_local.iter().map(|it| (it.local_path.as_deref().unwrap(), *it)).collect();
                charts.extend(get_data().collection_by_index(fav_index).charts.iter().filter_map(|it| {
                    if let Some(item) = local_chart_map.get(&*it.path) {
                        local_matcher(item).then(|| ChartDisplayItem::new(Some((*item).clone()), None))
                    } else if let Some(chart) = it.info.as_ref() {
                        search_by_id
                            .map_or_else(
                                || chart.info.name.to_ascii_lowercase().contains(&self.search_str.to_ascii_lowercase()),
                                |search_id| chart.info.id == Some(search_id),
                            )
                            .then(|| {
                                ChartDisplayItem::new(
                                    Some(ChartItem {
                                        info: chart.info.clone(),
                                        illu: Illustration::from_file_thumbnail(chart.illustration.clone()),
                                        local_path: None,
                                        chart_type: ChartType::Downloaded,
                                    }),
                                    None,
                                )
                            })
                    } else if let Some(local_path) = it.find_local_path().unwrap() {
                        let item = local_chart_map.get(&*local_path).unwrap();
                        local_matcher(item).then(|| ChartDisplayItem::new(Some((*item).clone()), None))
                    } else {
                        warn!("No info found for chart ref {it:?}");
                        None
                    }
                }));
                self.current_order.apply(&mut charts, |it| it.chart.as_ref().unwrap());
                if self.order_rev {
                    charts.reverse();
                }
            } else {
                if cfg!(closed) {
                    charts.push(ChartDisplayItem::new(None, None));
                }
                charts.extend(
                    charts_local
                        .iter()
                        .filter(|it| local_matcher(it))
                        .map(|it| ChartDisplayItem::new(Some((*it).clone()), None)),
                )
            }
            list.view.set(s.t, charts);
        }
    }

    fn on_order_update(&mut self, s: &mut SharedState) {
        let list = self.tabs.selected_mut();
        if list.ty == ChartListType::Local {
            self.sync_local(s);
        } else {
            self.current_page = 0;
            self.load_online();
        }
    }

    fn check_fav_page(&mut self, s: &mut SharedState) {
        if let Some(result) = FAV_PAGE_RESULT.with(|it| it.borrow_mut().take()) {
            self.current_fav_index = result;
            self.sync_local(s);
        }
    }

    fn update_order_meta_menu_options(&mut self) {
        self.order_meta_menu.set_options(vec![
            tl!("order-by", "order" => self.current_order.label()),
            if self.order_rev { tl!("order-desc") } else { tl!("order-asc") }.into(),
        ]);
    }

    fn get_move_fav_menu_options(&mut self) -> Option<Vec<String>> {
        let sel = self.tabs.selected().view.multi_select.as_ref()?;
        if sel.is_empty() {
            return None;
        }
        let data = get_data();
        let mut options = Vec::new();
        self.manage_fav_menu_options.clear();
        for uuid in data.collection_uuids() {
            let col = data.collection_info(uuid);
            if !col.is_owned() {
                continue;
            }
            let paths = col.charts.iter().collect::<HashSet<_>>();
            let all_in = sel.iter().all(|chart_ref| paths.contains(chart_ref));
            self.manage_fav_menu_options.push((*uuid, all_in));
            options.push(format!("{} {}", if all_in { '\u{2713}' } else { ' ' }, col.name));
        }
        Some(options)
    }
}

pub struct ExportConfig {
    pub file: File,
    pub deleter: Box<dyn FnOnce() -> io::Result<()> + Send>,
}

#[derive(Serialize, Deserialize)]
pub struct ExportInfo {
    pub exported_at: DateTime<Utc>,
    pub version: String,
}

static EXPORT_CONFIG: Mutex<Option<io::Result<ExportConfig>>> = Mutex::new(None);
#[cfg(target_os = "ios")]
static EXPORT_PICKER_PATH: Mutex<Option<String>> = Mutex::new(None);

#[cfg(target_os = "ios")]
fn present_export_picker(path: String) {
    use objc2::{available, define_class, rc::Retained, runtime::ProtocolObject, MainThreadMarker, MainThreadOnly};
    use objc2_foundation::{NSArray, NSObject, NSObjectProtocol, NSString, NSURL};
    use objc2_ui_kit::{UIDocumentPickerDelegate, UIDocumentPickerViewController};

    thread_local! {
        static DELEGATE: RefCell<Option<Retained<PickerDelegate>>> = const { RefCell::new(None) };
    }

    define_class! {
        // SAFETY:
        // - The superclass NSObject does not have any subclassing requirements.
        // - `PickerDelegate` does not implement `Drop`.
        #[unsafe(super = NSObject)]
        #[thread_kind = MainThreadOnly]
        struct PickerDelegate;

        // SAFETY: `NSObjectProtocol` has no safety requirements.
        unsafe impl NSObjectProtocol for PickerDelegate {}

        // SAFETY: `UIDocumentPickerDelegate` has no safety requirements.
        unsafe impl UIDocumentPickerDelegate for PickerDelegate {
            // SAFETY: The signature is correct.
            #[unsafe(method(documentPicker:didPickDocumentsAtURLs:))]
            fn did_pick_documents_at_urls(&self, _controller: &UIDocumentPickerViewController, _urls: &NSArray<NSURL>) {
                show_message(tl!("exported")).ok();
            }
        }
    }

    impl PickerDelegate {
        fn new(mtm: MainThreadMarker) -> Retained<Self> {
            let this = Self::alloc(mtm).set_ivars(());
            unsafe { objc2::msg_send![super(this), init] }
        }
    }

    let mtm = MainThreadMarker::new().unwrap();

    let url = NSURL::fileURLWithPath(&NSString::from_str(&path));
    let urls = NSArray::from_retained_slice(&[url]);
    let picker = UIDocumentPickerViewController::alloc(mtm);
    let picker = if available!(ios = 14.0.0) {
        UIDocumentPickerViewController::initForExportingURLs_asCopy(picker, &urls, true)
    } else {
        #[allow(deprecated)]
        {
            use objc2_ui_kit::UIDocumentPickerMode;
            UIDocumentPickerViewController::initWithURLs_inMode(picker, &urls, UIDocumentPickerMode::ExportToService)
        }
    };
    let dlg_obj = PickerDelegate::new(mtm);
    picker.setDelegate(Some(ProtocolObject::from_ref(&*dlg_obj)));
    DELEGATE.with(|it| *it.borrow_mut() = Some(dlg_obj));

    if let Some(controller) = inputbox::backend::IOS::get_top_view_controller(mtm) {
        controller.presentViewController_animated_completion(&picker, true, None);
    } else {
        show_error(Error::msg("Failed to present export dialog"));
    }
}

pub fn request_export(suggested_name: String) {
    cfg_if::cfg_if! {
        if #[cfg(target_os = "android")] {
            unsafe {
                let env = miniquad::native::attach_jni_env();
                let ctx = ndk_context::android_context().context();
                let class = (**env).GetObjectClass.unwrap()(env, ctx);
                let method =
                    (**env).GetMethodID.unwrap()(env, class, c"showExportDialog".as_ptr() as _, c"(Ljava/lang/String;)V".as_ptr() as _);
                let url = std::ffi::CString::new(suggested_name).unwrap();
                (**env).CallVoidMethod.unwrap()(
                    env,
                    ctx,
                    method,
                    (**env).NewStringUTF.unwrap()(env, url.as_ptr()),
                );
            }
        } else if #[cfg(target_os = "ios")] {
            use objc2_foundation::NSTemporaryDirectory;

            let dir = NSTemporaryDirectory();
            let output_path = std::path::PathBuf::from(dir.to_string()).join(&suggested_name);
            let output_path_str = output_path.to_string_lossy().to_string();
            let config = File::create(&output_path).map(|file| {
                let delete_path = output_path.clone();
                ExportConfig {
                    file,
                    deleter: Box::new(move || {
                        *EXPORT_PICKER_PATH.lock().unwrap() = None;
                        std::fs::remove_file(delete_path)
                    }),
                }
            });
            if config.is_ok() {
                EXPORT_PICKER_PATH.lock().unwrap().replace(output_path_str);
            }
            EXPORT_CONFIG.lock().unwrap().replace(config);
        } else if #[cfg(target_env = "ohos")] {
            miniquad::native::call_request_callback(format!("{{\"action\":\"request_export\",\"filename\":\"{}\"}}", suggested_name));
        } else {
            if let Some(output_path) = rfd::FileDialog::new().set_title(tl!("multi-export-title")).set_file_name(&suggested_name).save_file() {
                let config = File::create(&output_path).map(|file| ExportConfig {
                    file,
                    deleter: Box::new(move || std::fs::remove_file(output_path)),
                });
                EXPORT_CONFIG.lock().unwrap().replace(config);
            }
        }
    }
}

pub fn take_export() -> Option<io::Result<ExportConfig>> {
    EXPORT_CONFIG.lock().unwrap().take()
}
pub fn resolve_export() {
    #[cfg(target_os = "ios")]
    {
        if let Some(path) = EXPORT_PICKER_PATH.lock().unwrap().clone() {
            present_export_picker(path);
        } else {
            show_message(tl!("exported")).ok();
        }
    }
    #[cfg(not(target_os = "ios"))]
    show_message(tl!("exported")).ok();
}

#[cfg(target_os = "android")]
fn delete_uri(uri: Global<JObject<'static>>) {
    JavaVM::singleton()
        .unwrap()
        .attach_current_thread(|env| -> jni::errors::Result<()> {
            let ctx = ndk_context::android_context().context();
            let ctx = unsafe { JObject::from_raw(env, ctx as _) };
            env.call_method(ctx, jni_str!("deleteUri"), jni_sig!("(Landroid/net/Uri;)V"), &[uri.as_ref().into()])?;
            Ok(())
        })
        .unwrap();
}

#[cfg(target_os = "android")]
#[export_name = "Java_quad_1native_QuadNative_processExportFd"]
extern "system" fn process_export_fd(mut env: EnvUnowned, _: jni::objects::JClass, uri: jni::objects::JObject, fd: jni::sys::jint) {
    use std::os::fd::FromRawFd;
    env.with_env(|env| -> jni::errors::Result<()> {
        let uri = env.new_global_ref(uri)?;
        let file = unsafe { File::from_raw_fd(fd as _) };
        EXPORT_CONFIG.lock().unwrap().replace(Ok(ExportConfig {
            file,
            deleter: Box::new(|| {
                delete_uri(uri);
                Ok(())
            }),
        }));
        Ok(())
    })
    .resolve::<jni::errors::ThrowRuntimeExAndDefault>()
}

#[cfg(target_env = "ohos")]
mod ohos_export {
    use super::*;
    use napi_derive_ohos::napi;
    #[napi]
    #[allow(dead_code)]
    pub fn process_export_fd_ohos(fd: u32) {
        use std::os::fd::FromRawFd;
        let file = unsafe { File::from_raw_fd(fd as _) };
        EXPORT_CONFIG.lock().unwrap().replace(Ok(ExportConfig {
            file,
            deleter: Box::new(|| Ok(())),
        }));
    }
}

impl Page for LibraryPage {
    fn label(&self) -> Cow<'static, str> {
        tl!("label")
    }

    fn enter(&mut self, s: &mut SharedState) -> Result<()> {
        if FAV_UPDATED.swap(false, Ordering::SeqCst) {
            self.sync_local(s);
        }
        // 每次进页面重放一遍入场动画
        self.enter_t = s.t;
        Ok(())
    }

    fn on_result(&mut self, res: Box<dyn Any>, s: &mut SharedState) -> Result<()> {
        let _res = match res.downcast::<bool>() {
            Err(res) => res,
            Ok(delete) => {
                self.tabs.selected_mut().view.on_result(s.t, *delete);
                return Ok(());
            }
        };
        Ok(())
    }

    fn touch(&mut self, touch: &Touch, s: &mut SharedState) -> Result<bool> {
        let t = s.t;
        // 最佳谱面榜（T7.2）：导出中 / 榜单展开时独占触摸
        if self.best_board_export.busy() {
            return Ok(true);
        }
        if self.best_board.open {
            self.best_board.touch(touch, t);
            return Ok(true);
        }
        if self.sync_fav_task.is_some()
            || self.export_task.is_some()
            || self.multi_create_fav_task.is_some()
            || self.manage_fav_pre_task.is_some()
            || self.manage_fav_task.is_some()
            || self.refresh_local_fav_task.is_some()
        {
            return Ok(true);
        }
        let choose_cover = CHOOSE_COVER.load(Ordering::Relaxed);
        if !choose_cover {
            if self.order_menu.showing() {
                self.order_menu.touch(touch, t);
                return Ok(true);
            }
            if self.order_meta_menu.showing() {
                self.order_meta_menu.touch(touch, t);
                return Ok(true);
            }
            if self.manage_fav_menu.showing() {
                self.manage_fav_menu.touch(touch, t);
                return Ok(true);
            }
            if self.multi_operation_menu.showing() {
                self.multi_operation_menu.touch(touch, t);
                return Ok(true);
            }
            if self.multi_select_menu.showing() {
                self.multi_select_menu.touch(touch, t);
                return Ok(true);
            }
            if self.tags.touch(touch, t) {
                return Ok(true);
            }
            if self.rating.touch(touch, t) {
                return Ok(true);
            }
        }
        if self.tabs.selected().view.transiting() {
            return Ok(true);
        }
        // 课题模式（T7.3）：入口按钮 / 选择条 / 徽章弹窗优先吃掉触摸
        if !choose_cover && self.tabs.selected().ty == ChartListType::Local && self.challenge.touch(touch, t)? {
            if let Some(charts) = self.challenge.start.take() {
                self.challenge_scene = Some(NextScene::Overlay(Box::new(crate::scene::ChallengeScene::new(charts))));
            }
            return Ok(true);
        }
        // 最佳谱面榜（T7.2）入口
        if !choose_cover
            && self.tabs.selected().ty == ChartListType::Local
            && !self.order_menu.showing()
            && !self.order_meta_menu.showing()
            && !self.multi_operation_menu.showing()
            && !self.multi_select_menu.showing()
            && self.best_board.entry_touch(touch, t)
        {
            let (entries, rks) = self.rks.best_board();
            self.best_board.show(entries, rks, s);
            return Ok(true);
        }
        // 自定义 RKS（T7.4）：入口按钮在未展开时也要能被点到；展开后独占触摸。
        if !choose_cover && self.rks.touch(touch, t)? {
            if self.rks.take_refresh_request() {
                self.rks.refresh(&s.charts_local);
            }
            return Ok(true);
        }
        let charts_view = &mut self.tabs.selected_mut().view;
        if charts_view.touch(touch, t, s.rt)? {
            return Ok(true);
        }
        if choose_cover {
            return Ok(true);
        }
        if self.tabs.touch(touch, s.rt) {
            return Ok(true);
        }
        // New UI 头部排序组：默认 / 曲名 / 难度 / 评分
        const CHIP_ORDERS: [ChartOrder; 4] = [ChartOrder::Default, ChartOrder::Name, ChartOrder::Difficulty, ChartOrder::Rating];
        for (i, chip) in self.sort_chips.iter_mut().enumerate() {
            if chip.touch(touch, t) {
                if self.tabs.selected().ty == ChartListType::Local && CHIP_ORDERS[i] == ChartOrder::Rating {
                    // 本地谱面没有评分，选了也白选
                    return Ok(true);
                }
                self.current_order = CHIP_ORDERS[i];
                self.order_rev = matches!(self.current_order, ChartOrder::Default | ChartOrder::Rating);
                self.on_order_update(s);
                return Ok(true);
            }
        }
        if self.sort_rev_btn.touch(touch, t) {
            self.order_rev = !self.order_rev;
            self.on_order_update(s);
            return Ok(true);
        }
        if !matches!(self.tabs.selected().ty, ChartListType::Local) {
            if self.prev_page_btn.touch(touch, t) {
                if self.current_page != 0 {
                    self.current_page -= 1;
                    self.load_online();
                }
                return Ok(true);
            }
            if self.next_page_btn.touch(touch, t) {
                if self.current_page + 1 < self.total_page() {
                    self.current_page += 1;
                    self.load_online();
                }
                return Ok(true);
            }
        }

        match self.tabs.selected().ty {
            ChartListType::Local => {
                if self.tabs.selected().view.multi_select.is_none() {
                    if self.import_btn.touch(touch, t) {
                        request_file("_import");
                        return Ok(true);
                    }
                    if self.fav_btn.touch(touch, t) {
                        self.next_page = Some(NextPage::Overlay(Box::new(FavoritesPage::new(
                            self.icons.clone(),
                            self.rank_icons.clone(),
                            self.current_fav_index,
                            None,
                        ))));
                        return Ok(true);
                    }
                }
                if !self.search_str.is_empty() && self.search_clr_btn.touch(touch) {
                    button_hit();
                    self.search_str.clear();
                    self.sync_local(s);
                    return Ok(true);
                }
                if !self.search_clr_btn.contains(touch.position) && self.search_btn.touch(touch, t) {
                    request_input("search", InputBox::new().default_text(&self.search_str));
                    return Ok(true);
                }
            }
            ChartListType::Ranked | ChartListType::Special | ChartListType::Unstable => {
                if !self.search_str.is_empty() && self.search_clr_btn.touch(touch) {
                    button_hit();
                    self.search_str.clear();
                    self.current_page = 0;
                    self.load_online();
                    return Ok(true);
                }
                if !self.search_clr_btn.contains(touch.position) && self.search_btn.touch(touch, t) {
                    request_input("search", InputBox::new().default_text(&self.search_str));
                    return Ok(true);
                }
                if self.filter_btn.touch(touch, t) {
                    if self.filter_show_tag {
                        self.tags.enter(t);
                    } else {
                        self.rating.enter(t);
                    }
                    return Ok(true);
                }
            }
            ChartListType::Popular => {}
        }
        if self.tabs.selected_mut().view.multi_select.is_some() {
            if self.multi_operation_btn.touch(touch, t) {
                let mut options = vec!["multi-export", "multi-create-fav", "multi-manage-fav"];
                if self.tabs.selected_mut().view.allow_edit {
                    options.push("multi-delete");
                }
                self.multi_operation_menu
                    .set_options(options.iter().map(|it| tl!(*it).into_owned()).collect());
                self.multi_operation_options = options;
                self.need_show_multi_operation_menu = true;
                return Ok(true);
            }
            if self.multi_select_btn.touch(touch, t) {
                self.need_show_multi_select_menu = true;
                return Ok(true);
            }
            if self.multi_select_cancel_btn.touch(touch, t) {
                self.tabs.selected_mut().view.multi_select = None;
                return Ok(true);
            }
        }
        if self.order_btn.touch(touch, t) {
            self.need_show_order_meta_menu = true;
            return Ok(true);
        }
        Ok(false)
    }

    fn update(&mut self, s: &mut SharedState) -> Result<()> {
        let t = s.t;

        // New UI：点卡片 → 打开曲目详情页；课题模式打开时改成把谱面塞进课题槽位
        if let Some(chart) = self.tabs.selected_mut().view.take_pending_detail() {
            if self.challenge.active && self.tabs.selected().ty == ChartListType::Local {
                self.challenge.select(chart);
            } else {
                self.next_page = Some(NextPage::Overlay(Box::new(SongDetailPage::new(
                    chart,
                    Arc::clone(&self.icons),
                    self.rank_icons.clone(),
                )?)));
            }
        }

        self.challenge.update(t);
        // 最佳谱面榜（T7.2）
        self.best_board.update(t);
        self.best_board_export.update();

        if let Some(chosen_cover) = CHOSEN_COVER.with(|it| it.borrow_mut().take()) {
            CHOOSE_COVER.store(false, Ordering::Relaxed);
            self.next_page = Some(NextPage::Overlay(Box::new(FavoritesPage::new(
                self.icons.clone(),
                self.rank_icons.clone(),
                self.current_fav_index,
                Some(chosen_cover),
            ))));
        }

        self.check_fav_page(s);

        if self.tabs.selected().ty == ChartListType::Local && self.current_order == ChartOrder::Rating {
            self.current_order = ChartOrder::Default;
            self.order_rev = true;
        }

        self.tags.update(t);
        self.rating.update(t);

        let is_local = self.tabs.selected().ty == ChartListType::Local;
        if self.tabs.changed() {
            self.tabs.selected_mut().view.reset_scroll();
            self.tabs.iter_mut().for_each(|it| it.view.multi_select = None);
            self.online_task = None;
            if is_local {
                self.sync_local(s);
            } else {
                self.current_page = 0;
                self.load_online();
            }
        }
        if cfg!(closed) && self.tabs.selected_mut().view.clicked_special {
            let icons = Arc::clone(&self.icons);
            self.next_page_task = Some(Box::pin(async move { Ok(NextPage::Overlay(Box::new(CollectionPage::new(icons).await?))) }));
            self.tabs.selected_mut().view.clicked_special = false;
        }
        if let Some(task) = &mut self.next_page_task {
            if let Some(res) = poll_future(task.as_mut()) {
                self.next_page = Some(res?);
                self.next_page_task = None;
            }
        }

        if self.tags.show_rating {
            self.tags.show_rating = false;
            self.filter_show_tag = false;
            self.rating.enter(t);
        } else if self.tags_last_show && !self.tags.showing() {
            self.current_page = 0;
            self.load_online();
        }
        if self.rating.show_tags {
            self.rating.show_tags = false;
            self.filter_show_tag = true;
            self.tags.enter(t);
        } else if self.rating_last_show && !self.rating.showing() {
            self.current_page = 0;
            self.load_online();
        }
        self.tags_last_show = self.tags.showing();
        self.rating_last_show = self.rating.showing();
        if let Some(task) = &mut self.online_task {
            if let Some(res) = task.take() {
                match res {
                    Err(err) => show_error(err.context(tl!("failed-to-load-online"))),
                    Ok(res) => {
                        self.online_total_page = res.2;
                        self.tabs.selected_mut().view.set(t, res.0);
                    }
                }
                self.online_task = None;
            }
        }
        self.order_menu.update(t);
        self.order_meta_menu.update(t);
        self.multi_operation_menu.update(t);
        self.multi_select_menu.update(t);
        for chart in &mut s.charts_local {
            chart.illu.settle(t);
        }
        self.tabs.selected_mut().view.can_refresh = !is_local || self.current_fav_index.is_some();
        if self.tabs.selected_mut().view.update(t)? {
            if is_local {
                if let Some(index) = self.current_fav_index {
                    let data = get_data();
                    let uuid = data.collection_uuids()[index];
                    let col = data.collection_by_index(index);
                    if let Some(col_id) = col.id {
                        if !data.config.offline_mode {
                            self.sync_fav_task = Some(Task::new(async move {
                                let resp: Collection = recv_raw(Client::get(format!("/collection/{col_id}"))).await?.json().await?;
                                Ok(Some(resp))
                            }));
                        }
                    } else {
                        let charts = col.charts.clone();
                        self.refresh_local_fav_task = Some(Task::new(async move {
                            let mut ids_str = String::new();
                            for chart in &charts {
                                if let Some(id) = chart.id() {
                                    ids_str.push_str(&id.to_string());
                                    ids_str.push(',');
                                }
                            }
                            let mut updated = charts;
                            if !ids_str.is_empty() {
                                ids_str.pop();
                                let resp: Vec<Chart> = recv_raw(Client::get(format!("/chart/multi-get?ids={ids_str}"))).await?.json().await?;
                                let mut id_to_chart = HashMap::new();
                                for chart in resp {
                                    id_to_chart.insert(chart.id, chart);
                                }
                                for chart in &mut updated {
                                    if let Some(id) = chart.id() {
                                        if let Some(info) = id_to_chart.get(&id) {
                                            chart.info = Some(Box::new(ChartRefChartInfo::from_chart(info)));
                                        }
                                    }
                                }
                            }
                            Ok((uuid, updated))
                        }));
                    }
                }
            } else {
                self.load_online();
            }
        }
        if self.tabs.selected_mut().view.need_update() {
            s.reload_local_charts();
            self.sync_local(s);
        }
        if let Some((id, text)) = take_input() {
            if id == "search" {
                self.search_str = text;
                if is_local {
                    self.sync_local(s);
                } else {
                    self.current_page = 0;
                    self.load_online();
                }
            } else if id == "rks:filter" {
                self.rks.set_filter(text);
            } else if id.starts_with("rks:") {
                if let Err(err) = self.rks.input(&id, text) {
                    show_error(err);
                }
            } else if id == "new_fav" {
                if text.is_empty() {
                    use crate::page::favorites::{tl as ftl, L10N_LOCAL};
                    show_message(ftl!("name-empty")).error();
                } else if let Err(err) = crate::censor::check_text(&text) {
                    show_message(err.to_string()).error();
                } else {
                    let charts_view = &mut self.tabs.selected_mut().view;
                    if let Some(mut selected) = charts_view.multi_select.clone() {
                        self.multi_create_fav_task = Some(Task::new(async move {
                            let mut ids_str = String::new();
                            for chart in &selected {
                                if let Some(id) = chart.id() {
                                    ids_str.push_str(&id.to_string());
                                    ids_str.push(',');
                                }
                            }
                            if !ids_str.is_empty() {
                                ids_str.pop();
                                let resp: Vec<Chart> = recv_raw(Client::get(format!("/chart/multi-get?ids={ids_str}"))).await?.json().await?;
                                let mut id_to_chart = HashMap::new();
                                for chart in resp {
                                    id_to_chart.insert(chart.id, Box::new(chart));
                                }
                                for chart in &mut selected {
                                    if let Some(id) = chart.id() {
                                        chart.info = Some(Box::new(ChartRefChartInfo::from_chart(id_to_chart.get(&id).unwrap())));
                                    }
                                }
                            }
                            Ok(CreateFavorite {
                                name: text,
                                charts: selected,
                            })
                        }));
                    }
                }
            } else {
                return_input(id, text);
            }
        }
        if let Some(task) = &mut self.multi_create_fav_task {
            if let Some(res) = task.take() {
                match res {
                    Err(err) => show_error(err),
                    Ok(result) => {
                        self.tabs.selected_mut().view.multi_select = None;
                        let data = get_data_mut();
                        let mut col = LocalCollection::new(result.name);
                        col.charts = result.charts;
                        data.push_collection(col)?;
                        let _ = save_data();
                        show_message(tl!("fav-created")).ok();
                        self.current_fav_index = Some(data.collection_uuids().len() - 1);
                        self.sync_local(s);
                    }
                }
                self.multi_create_fav_task = None;
            }
        }
        if self.delete_multi.swap(false, Ordering::Relaxed) {
            let selected = self.tabs.selected_mut().view.multi_select.take().unwrap();
            let selected = selected.into_iter().collect::<HashSet<_>>();
            let data = get_data_mut();
            let mut local_paths = HashSet::new();
            for chart in &selected {
                if let Some(path) = chart.find_local_path()? {
                    match std::fs::remove_dir_all(format!("{}/{path}", dir::charts()?)) {
                        Ok(_) => {}
                        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
                        Err(err) => return Err(err.into()),
                    }
                    local_paths.insert(path);
                }
            }
            data.charts.retain(|it| !local_paths.contains(it.local_path.as_str()));
            let _ = save_data();
            show_message(tl!("multi-deleted")).ok();
            s.reload_local_charts();
            self.sync_local(s);
        }
        if self.order_meta_menu.changed() {
            match self.order_meta_menu.selected() {
                0 => {
                    self.need_show_order_menu = true;
                }
                1 => {
                    self.order_rev = !self.order_rev;
                    self.update_order_meta_menu_options();
                    self.order_meta_menu.set_selected(usize::MAX);
                    self.on_order_update(s);
                }
                _ => {}
            }
        }
        if self.order_menu.changed() {
            self.current_order = self.order_menu_options[self.order_menu.selected()];
            self.order_rev = matches!(self.current_order, ChartOrder::Default | ChartOrder::Rating);
            self.order_meta_menu.set_selected(usize::MAX);
            self.update_order_meta_menu_options();
            self.on_order_update(s);
        }
        if self.multi_operation_menu.changed() {
            let charts_view = &mut self.tabs.selected_mut().view;
            let selected = charts_view.multi_select.as_mut().unwrap();
            match self.multi_operation_options[self.multi_operation_menu.selected()] {
                "multi-export" => {
                    self.multi_operation_menu.dismiss(t);
                    let mut paths = Vec::with_capacity(selected.len());
                    let mut non_existent = Vec::new();
                    for chart in selected {
                        match chart.find_local_path()? {
                            Some(path) => paths.push(path.into_owned()),
                            None => {
                                let mut charts = charts_view.charts.as_ref().unwrap().iter().filter_map(|it| it.chart.as_ref());
                                non_existent.push(charts.find(|it| &it.to_bare_ref() == chart).unwrap().info.name.clone());
                            }
                        }
                    }
                    if !non_existent.is_empty() {
                        Dialog::simple(tl!("multi-export-no-file", "charts" => non_existent.join(", "))).show();
                    } else {
                        self.export_paths = Some(paths);
                        request_export(format!("phira-export-{}.zip", chrono::Local::now().format("%Y%m%d-%H%M%S")));
                    }
                }
                "multi-create-fav" => {
                    self.multi_operation_menu.dismiss(t);
                    request_input("new_fav", InputBox::new());
                }
                "multi-manage-fav" => {
                    if let Some(options) = self.get_move_fav_menu_options() {
                        self.need_show_manage_fav_menu = true;
                        self.manage_fav_menu.set_options(options);
                        self.multi_operation_menu.set_selected(usize::MAX);
                    } else {
                        // Do nothing
                    }
                }
                "multi-delete" => {
                    self.multi_operation_menu.dismiss(t);
                    confirm_dialog(ttl!("del-confirm"), tl!("multi-delete-confirm", "count" => selected.len()), self.delete_multi.clone());
                }
                _ => {}
            }
        }
        if self.multi_select_menu.changed() {
            let charts_view = &mut self.tabs.selected_mut().view;
            let sel = charts_view.multi_select.as_mut().unwrap();
            let charts = charts_view.charts.as_ref().unwrap();
            match self.multi_select_menu.selected() {
                0 => {
                    sel.clear();
                    sel.extend(charts.iter().filter_map(|it| it.chart.as_ref()).map(ChartItem::to_bare_ref));
                }
                1 => {
                    let old_sel = mem::take(sel).into_iter().collect::<HashSet<_>>();
                    for chart in charts {
                        if let Some(chart) = &chart.chart {
                            let r = chart.to_bare_ref();
                            if !old_sel.contains(&r) {
                                sel.push(r);
                            }
                        }
                    }
                }
                _ => {}
            }
        }
        if self.manage_fav_menu.changed() {
            let (uuid, all_in_fav) = self.manage_fav_menu_options[self.manage_fav_menu.selected()];
            self.manage_fav_menu.set_selected(usize::MAX);
            let add = !all_in_fav;
            let mut charts = self.tabs.selected().view.multi_select.as_ref().unwrap().clone();
            self.manage_fav_pre_task = Some(Task::new(async move {
                if add {
                    let mut ids_str = String::new();
                    for chart in &charts {
                        if let Some(id) = chart.id() {
                            ids_str.push_str(&id.to_string());
                            ids_str.push(',');
                        }
                    }
                    if !ids_str.is_empty() {
                        ids_str.pop();
                        let resp: Vec<Chart> = recv_raw(Client::get(format!("/chart/multi-get?ids={ids_str}"))).await?.json().await?;
                        let mut id_to_chart = HashMap::new();
                        for chart in resp {
                            id_to_chart.insert(chart.id, chart);
                        }
                        for chart in &mut charts {
                            if let Some(id) = chart.id() {
                                if let Some(info) = id_to_chart.get(&id) {
                                    chart.info = Some(Box::new(ChartRefChartInfo::from_chart(info)));
                                }
                            }
                        }
                    }
                }
                Ok(ManageFavorite { uuid, charts, add })
            }));
        }
        if JUST_LOADED_TOS.fetch_and(false, Ordering::Relaxed) {
            check_read_tos_and_policy(false, false);
        }
        let list = self.tabs.selected_mut();
        let view = &mut list.view;
        if let Some((from, to)) = view.take_movement() {
            if self.current_order != ChartOrder::Default && self.current_fav_index.is_none() {
                show_message(tl!("order-update-failed-sort")).error();
                return Ok(());
            }
            let data = get_data_mut();
            if let Some(index) = self.current_fav_index {
                let uuid = data.collection_uuids()[index];
                let mut col = data.collection_info(&uuid).as_ref().clone();
                let online = col.id.is_some();
                let chart = col.charts.remove(from);
                col.charts.insert(to, chart);
                data.set_collection_info(&uuid, col)?;
                let _ = save_data();
                if online && !data.config.offline_mode {
                    if let Some(task) = FavoritesPage::sync_to_cloud_task(index, false) {
                        self.sync_fav_task = Some(task);
                    }
                }
            } else {
                if self.order_rev {
                    let chart = data.charts.remove(data.charts.len() - from - 1);
                    data.charts.insert(data.charts.len() - to, chart);
                } else {
                    let chart = data.charts.remove(from);
                    data.charts.insert(to, chart);
                }
                let _ = save_data();
                s.reload_local_charts();
            }
            show_message(tl!("order-updated")).ok();
        }
        view.allow_edit(
            list.ty == ChartListType::Local
                && self.search_str.is_empty()
                && self.current_fav_index.is_none_or(|it| get_data().collection_by_index(it).is_owned()),
        );

        if let Some(task) = &mut self.refresh_local_fav_task {
            if let Some(res) = task.take() {
                match res {
                    Err(err) => show_error(err),
                    Ok((uuid, charts)) => {
                        let data = get_data();
                        let mut col = data.collection_info(&uuid).as_ref().clone();
                        col.charts = charts;
                        data.set_collection_info(&uuid, col)?;
                        let _ = save_data();
                        show_message(tl!("fav-synced")).ok();
                        self.sync_local(s);
                    }
                }
                self.refresh_local_fav_task = None;
            }
        }
        if let Some(task) = &mut self.sync_fav_task {
            if let Some(res) = task.take() {
                match res {
                    Err(err) => show_error(err.context(tl!("fav-sync-failed"))),
                    Ok(Some(col)) => {
                        let data = get_data();
                        let uuid = data.collection_uuids()[self.current_fav_index.unwrap()];
                        let local = data.collection_info(&uuid);
                        data.set_collection_info(&uuid, local.merge(&col))?;
                        let _ = save_data();
                        show_message(tl!("fav-synced")).ok();
                        self.sync_local(s);
                    }
                    Ok(None) => {
                        use crate::page::favorites::{tl as ftl, L10N_LOCAL};
                        confirm_dialog(ftl!("sync-to-cloud"), ftl!("sync-outdated"), self.force_sync_to_cloud.clone());
                    }
                }
                self.sync_fav_task = None;
            }
        }
        if self.force_sync_to_cloud.swap(false, Ordering::SeqCst) {
            if let Some(index) = self.current_fav_index {
                if let Some(task) = FavoritesPage::sync_to_cloud_task(index, true) {
                    self.sync_fav_task = Some(task);
                }
            }
        }
        if let Some(config) = take_export() {
            fn export_inner(paths: Vec<String>, output: File, progress: Arc<AtomicU32>) -> Result<()> {
                let charts = dir::charts()?;
                let mut zip = zip::ZipWriter::new(BufWriter::new(output));
                let options = zip::write::SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Stored)
                    .unix_permissions(0o755)
                    .last_modified_time(chrono::Utc::now().naive_utc().try_into().unwrap_or_default());
                for (i, name) in paths.iter().enumerate() {
                    zip.start_file(format!("{name}.zip"), options)?;
                    let mut chart_bytes = Vec::new();
                    compress_folder(Path::new(&format!("{charts}/{name}")), &mut Cursor::new(&mut chart_bytes))?;
                    zip.write_all(&chart_bytes)?;
                    progress.store(i as u32 + 1, Ordering::Relaxed);
                }

                zip.start_file("export.json", options.compression_method(zip::CompressionMethod::Deflated))?;
                let info = ExportInfo {
                    exported_at: Utc::now(),
                    version: env!("CARGO_PKG_VERSION").to_owned(),
                };
                serde_json::to_writer(&mut zip, &info)?;

                zip.finish()?;
                Ok(())
            }

            match config {
                Err(err) => show_error(err.into()),
                Ok(config) => {
                    if let Some(paths) = self.export_paths.take() {
                        self.export_total = paths.len();
                        let (tx, rx) = mpsc::sync_channel(1);
                        let progress = self.export_progress.clone();
                        progress.store(0, Ordering::SeqCst);
                        std::thread::spawn(move || {
                            let result = export_inner(paths, config.file, progress);
                            if result.is_err() {
                                if let Err(err) = (config.deleter)() {
                                    warn!("failed to delete export file: {:?}", err);
                                }
                            }
                            let _ = tx.send(result);
                        });
                        self.export_task = Some(rx);
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
                    self.tabs.selected_mut().view.multi_select = None;
                    self.export_task = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
                Err(mpsc::TryRecvError::Disconnected) => {
                    show_error(Error::msg("Export thread panicked"));
                    self.export_task = None;
                }
            }
        }
        if let Some(task) = &mut self.manage_fav_pre_task {
            if let Some(res) = task.take() {
                match res {
                    Err(err) => show_error(err),
                    Ok(ManageFavorite { uuid, charts, add }) => {
                        let data = get_data();
                        let col = data.collection_info(&uuid).as_ref().clone();
                        match col.update(uuid, &charts, add) {
                            CollectionUpdate::Unchanged => {}
                            CollectionUpdate::Updated { sync_task, add } => {
                                if let Some(task) = sync_task {
                                    self.manage_fav_task = Some(task);
                                } else if add {
                                    show_message(tl!("multi-added-to-fav")).duration(1.).ok();
                                } else {
                                    show_message(tl!("multi-removed-from-fav")).duration(1.).ok();
                                }
                            }
                        }
                        let _ = save_data();
                        if self.current_fav_index.is_some_and(|it| data.collection_uuids()[it] == uuid) {
                            self.manage_fav_menu.dismiss(s.t);
                            self.multi_operation_menu.dismiss(s.t);
                            self.tabs.selected_mut().view.multi_select = None;
                            self.sync_local(s);
                        } else {
                            let options = self.get_move_fav_menu_options().unwrap();
                            self.manage_fav_menu.set_options(options);
                        }
                    }
                }
                self.manage_fav_pre_task = None;
            }
        }
        if let Some(task) = &mut self.manage_fav_task {
            if let Some(res) = task.take() {
                match res {
                    Err(err) => {
                        show_error(err);
                    }
                    Ok((col, added)) => {
                        let data = get_data();
                        if let Some(uuid) = data.collection_uuids().iter().find(|it| data.collection_info(it).id == Some(col.id)) {
                            let uuid = *uuid;
                            let local = data.collection_info(&uuid);
                            data.set_collection_info(&uuid, local.merge(&col))?;
                        }
                        if added {
                            show_message(tl!("multi-added-to-fav")).ok();
                        } else {
                            show_message(tl!("multi-removed-from-fav")).ok();
                        }
                    }
                }
                self.manage_fav_task = None;
            }
        }

        Ok(())
    }

    fn render(&mut self, ui: &mut Ui, s: &mut SharedState) -> Result<()> {
        self.check_fav_page(s);

        let t = s.t;
        let rt = s.rt;
        let m = newui::Metrics::new(ui);
        let th = newui::theme();
        let chosen = self.tabs.selected().ty;
        let multi_select = self.tabs.selected().view.multi_select.is_some();
        let is_local = chosen == ChartListType::Local;

        // 课题模式（T7.3）：开局重置本帧的入口可用状态；切到别的分页就关掉选择条
        self.challenge.begin_render();
        if !is_local && self.challenge.active {
            self.challenge.close();
        }

        // 自定义 RKS（T7.4）：编辑器展开时整页让位给它
        self.rks.begin_render();
        self.best_board.entry_active = false;
        if self.rks.editing {
            s.render_fader(ui, |ui| {
                let area = ui.content_rect();
                self.rks.render_overlay(ui, t, &m, area);
                Ok::<(), anyhow::Error>(())
            })?;
            return Ok(());
        }

        // ---- 版式：从顶到底单纯堆叠（头部行 → 分类条 / 列 + 网格），任何比例都不交叉 ----
        //
        // 左侧活动栏删除后这里重排过一遍：
        //   * 左右边距统一取 content_rect() 的左/右缘（∓0.94，各 3% 屏宽）—— 内容真正左移；
        //   * 顶栏的搜索框**永远排在返回键右边**（返回键是 MainScene 画的，页面必须避开），
        //     不允许再出现「搜索框压在返回键下面」；
        //   * 竖屏（compact）把一行拆成三行：①返回键 + 搜索框 ②右侧图标钮 ③横向分类条，
        //     原来全挤在一行，搜索框和第三个图标钮必然重叠。
        let sb = m.statusbar(ui);
        let top = -ui.top;
        let bottom = sb.y - m.u(4.);
        let back = newui::back_rect(ui, &m);
        let compact = m.compact;
        // 顶部三行整体放大。
        //
        // 原来第一行 58px、第二行（右侧图标钮）46px、分类标签条 42px —— 用户反馈
        // 「都小」，所以统一按 1.3 倍放大。所有相关的尺寸都乘 `lib_scale`，
        // 这样图标 + 字号 + 行高会一起变大，不会出现「框大了字还小」的错位。
        // 图标基准 18px（原设计稿值），放大后作为图标与内嵌文字的共同基准。
        let lib_scale = if compact { 1.3 } else { 1.15 };
        let icon_base = 18. * lib_scale;
        let head_h = m.u(58. * lib_scale);
        let cr = ui.content_rect();
        let right = cr.right();
        let x0 = cr.x;

        // 分类条方向：竖屏横排（铺满整宽、只占一行），横屏竖排（左侧分类栏）
        self.tabs.set_horizontal(compact);
        // 标签文字跟着顶部三行一起放大（量与画同步，栏宽才不会和文字对不上）
        self.tabs.set_label_scale(lib_scale);

        let header;
        let strip;
        let mut grid;
        if compact {
            // ①②③ 三行堆叠
            let head_x = (back.right() + m.u(10.)).min(right - m.u(120.));
            header = Rect::new(head_x, top + m.u(8.), (right - head_x).max(m.u(120.)), head_h);
            let row2_y = header.bottom() + m.u(8.);
            let row2_h = m.u(46. * lib_scale);
            let strip_y = row2_y + row2_h + m.u(10.);
            let strip_h = m.u(42. * lib_scale);
            strip = Rect::new(x0, strip_y, right - x0, strip_h);
            let gy = strip.bottom() + m.u(10.);
            grid = Rect::new(x0, gy, right - x0, (bottom - gy).max(m.u(40.)));
        } else {
            let head_x = (back.right() + m.u(10.)).max(x0);
            header = Rect::new(head_x, top + m.u(8.), (right - head_x).max(m.u(120.)), head_h);
            let strip_y = header.bottom() + m.u(10.);
            // 分类栏宽度按**真实标签宽度**算，别硬编码 160px（比最长标签宽出一截，显得又粗又空）
            let max_lbl = self.tabs.label_widths(ui).into_iter().fold(0f32, f32::max);
            let strip_w = (max_lbl + m.u(44.)).clamp(m.u(96.), m.u(180.));
            strip = Rect::new(x0, strip_y, strip_w, (bottom - strip_y).max(m.u(40.)));
            let gx = strip.right() + m.u(10.);
            grid = Rect::new(gx, strip_y, (right - gx).max(m.u(120.)), (bottom - strip_y).max(m.u(40.)));
        }
        if !is_local {
            grid.h -= m.u(30.);
        }
        // 课题模式（T7.3）：选择条占一行高度，把网格整体下压
        // （放在 HUD 自定义布局之前，grid 被 HUD 覆盖时选择条按默认位置显示）
        let challenge_strip = if self.challenge.active && is_local {
            let h = m.u(crate::page::challenge::SELECTOR_HEIGHT_PX);
            let r = Rect::new(grid.x, grid.y, grid.w, h);
            grid.y += h + m.u(8.);
            grid.h = (grid.h - h - m.u(8.)).max(m.u(40.));
            Some(r)
        } else {
            None
        };

        // HUD 自定义布局：谱面列表大框 / 顶部工具行
        const SLOT_LIST: crate::hud::SlotDef = crate::hud::SlotDef::centered("list", [0., 0., 1.67, 1.2], crate::hud::Cap(true, true, true));
        if crate::hud::has("library", "list") {
            grid = crate::hud::slot(ui, "library", SLOT_LIST);
        } else {
            crate::hud::register("library", SLOT_LIST, grid);
        }
        const SLOT_TOPBAR: crate::hud::SlotDef = crate::hud::SlotDef::centered("topbar", [0., 0., 0.09, 0.09], crate::hud::Cap(true, true, true));
        let header = if crate::hud::has("library", "topbar") {
            crate::hud::slot(ui, "library", SLOT_TOPBAR)
        } else {
            crate::hud::register("library", SLOT_TOPBAR, header);
            header
        };
        // 紧凑模式：图标钮另起一行（跟在顶栏下方）；横屏：与搜索框同一行（heads[1] == heads[0]）
        let action_row = if compact {
            Rect::new(x0, header.bottom() + m.u(8.), (right - x0).max(m.u(120.)), m.u(46. * lib_scale))
        } else {
            header
        };
        let heads = [header, action_row];

        // 状态栏：曲目计数
        let count = self
            .tabs
            .selected()
            .view
            .charts
            .as_ref()
            .map(|c| c.iter().filter(|i| i.chart.is_some()).count())
            .unwrap_or(0);
        newui::set_status_extra(Some((format!("{count} 首曲目"), String::new())));

        // 入场：头部下坠 + 网格淡入
        let enter = (t - self.enter_t).max(0.);

        s.render_fader(ui, |ui| {
            self.tabs.render(ui, rt, "library", strip, grid, |ui, list| {
                list.view.render(ui, grid.feather(-m.u(6.)), t);
                Ok(())
            })?;
            let p = newui::enter_p(enter, 0.05);
            ui.alpha(p, |ui| {
                ui.scope(|ui| {
                    ui.dy((1. - p) * m.u(12.));
                    self.render_head(ui, t, &m, &th, heads, compact, chosen, multi_select, is_local);
                });
            });

            // 翻页
            if !is_local {
                let p = newui::enter_p(enter, 0.15);
                ui.alpha(p, |ui| self.render_pagination(ui, t, &m, &th, grid));
            }

            // 课题模式（T7.3）：选择条（叠在网格上方）。
            // 注意每帧都要调一次 —— 徽章弹窗（BadgePicker）的渲染也在里面。
            {
                let enabled = is_local && !multi_select && !CHOOSE_COVER.load(Ordering::Relaxed);
                let cs = challenge_strip.unwrap_or(Rect::new(0., 0., 0., 0.));
                self.challenge.render(ui, t, enabled, &m, cs);
            }
            Ok::<(), anyhow::Error>(())
        })?;
        self.order_menu.render(ui, t, 1.);
        self.order_meta_menu.render(ui, t, 1.);
        if self.need_show_order_menu {
            self.need_show_order_menu = false;
            self.order_menu.set_bottom(true);
            self.order_menu.set_selected(
                self.order_menu_options
                    .iter()
                    .position(|&it| it == self.current_order)
                    .unwrap_or(usize::MAX),
            );
            self.order_menu
                .set_options(self.order_menu_options.iter().map(|it| it.label().into_owned()).collect());

            let mut r = self.order_meta_menu.rect();
            r.w = 0.3;
            r.x -= r.w + 0.02;
            r.h = 0.4;
            self.order_menu.show(ui, t, r);
        }
        self.multi_select_menu.render(ui, t, 1.);
        self.multi_operation_menu.render(ui, t, 1.);
        if self.need_show_manage_fav_menu {
            self.need_show_manage_fav_menu = false;
            self.manage_fav_menu.set_selected(usize::MAX);
            let mut r = self.multi_operation_menu.rect();
            r.w = 0.3;
            r.x -= r.w + 0.02;
            r.h = 0.4;
            self.manage_fav_menu.show(ui, t, r);
        }
        self.manage_fav_menu.render(ui, t, 1.);
        self.tags.render(ui, t);
        self.rating.render(ui, t);
        // 最佳谱面榜（T7.2）：整屏覆盖 + 长图导出
        self.best_board.render(ui, t);
        self.best_board_export.render(&mut self.best_board, ui, t);
        Ok(())
    }

    fn render_top(&mut self, ui: &mut Ui, s: &mut SharedState) -> Result<()> {
        let t = s.t;
        self.tabs.selected_mut().view.render_top(ui, t);
        if self.sync_fav_task.is_some() {
            ui.full_loading_simple(t);
        }
        if self.export_task.is_some() {
            let current = self.export_progress.load(Ordering::Relaxed);
            let total = self.export_total;
            ui.full_loading(tl!("multi-exporting", "current" => current, "total" => total), t);
        }
        if self.multi_create_fav_task.is_some()
            || self.manage_fav_pre_task.is_some()
            || self.manage_fav_task.is_some()
            || self.refresh_local_fav_task.is_some()
        {
            ui.full_loading_simple(t);
        }
        Ok(())
    }

    fn next_page(&mut self) -> NextPage {
        self.next_page.take().unwrap_or_default()
    }

    /// 返回键：依次关掉长图导出 / 最佳谱面榜 / 课题选择条 / RKS 编辑器。
    fn on_back_pressed(&mut self, _s: &mut SharedState) -> bool {
        if self.best_board_export.cancel() {
            return true;
        }
        if self.best_board.close() {
            return true;
        }
        if self.challenge.close() {
            self.challenge_scene = None;
            return true;
        }
        self.rks.back()
    }

    fn next_scene(&mut self, _s: &mut SharedState) -> NextScene {
        // 课题模式（T7.3）：选满三张、点了「开始课题」后接管整个场景
        if let Some(scene) = self.challenge_scene.take() {
            return scene;
        }
        self.tabs.selected_mut().view.next_scene().unwrap_or_default()
    }
}

impl LibraryPage {
    // ============================================================
    // New UI 曲库头部（搜索框 + 排序 chips + 操作按钮）与翻页条
    // ============================================================

    /// 头部：搜索框 + 排序组 + 右侧操作按钮。
    ///
    /// `heads[0]` = 搜索框所在行（顶栏），`heads[1]` = 图标钮所在行。
    /// 横屏两者是同一行；竖屏（compact）图标钮另起一行，避免和搜索框叠在一起。
    #[allow(clippy::too_many_arguments)]
    fn render_head(
        &mut self,
        ui: &mut Ui,
        t: f32,
        m: &Metrics,
        th: &newui::Theme,
        heads: [Rect; 2],
        compact: bool,
        chosen: ChartListType,
        multi_select: bool,
        is_local: bool,
    ) {
        let tool_row = heads[0];
        let icons_row = heads[1];
        let _ = chosen;
        // 顶部三行的放大倍率 / 图标基准（与 `render` 里 head_h 用的同一套公式，
        // 保证「行高放大了、里面的图标和文字也跟着放大」）。
        let lib_scale = if compact { 1.3 } else { 1.15 };
        let icon_base = 18. * lib_scale;
        // 悬停判定（与 `Touch::position` 同一套世界坐标）
        let (mx, my) = newui::mouse_world();
        let mouse = vec2(mx, my);

        // 右侧文字入口（课题 / 自定义 RKS / 最佳榜）只在本地页、非多选、非选封面时出现
        let show_entries = is_local && !multi_select && !CHOOSE_COVER.load(Ordering::Relaxed);
        // 右侧操作区总宽：最多 3 个图标钮 + 间距，再加三个文字入口。
        // 之前只预留 170px，导致新入口压到搜索框上（用户报的「布局塌陷」），这里一次算准。
        // 图标钮按放大后的尺寸算，否则改成 1.3 倍后会重新压到搜索框。
        let icon_btn = 46. * lib_scale;
        let icons_w = m.u(icon_btn) * 3. + m.u(6. * lib_scale) * 3.;
        let entry_w = if show_entries {
            m.u(160.) + m.u(120.) + m.u(110.) + m.u(12.)
        } else {
            0.
        };
        // 竖屏时图标钮在**另一行**，搜索框可以放心用满整行，不再被它们挤成 120px 宽的一条。
        let actions_w = if compact { 0. } else { icons_w + entry_w + m.u(12.) };
        let chips_est = m.u(250.);
        let base_w = (tool_row.w - actions_w).max(m.u(120.));
        // 空间不够时先砍 chips（窄屏 / 大倍率窗口都走这条）
        let show_chips = !compact && base_w > m.u(614.);

        // ---- 搜索框（横屏时留出右侧操作按钮的宽度；竖屏用满整行） ----
        let sr: Rect;
        {
            let search_w = if compact {
                base_w.max(m.u(120.))
            } else if show_chips {
                m.u(340.).min(base_w - chips_est - m.u(24.))
            } else {
                (base_w - m.u(12.)).max(m.u(120.))
            };
            // HUD 自定义布局：搜索框
            sr = crate::hud::slot_or(
                ui,
                "library",
                "search",
                crate::hud::Cap(true, true, true),
                Rect::new(tool_row.x, tool_row.center().y - m.u(22. * lib_scale), search_w, m.u(44. * lib_scale)),
            );
            self.search_btn.build(ui, t, sr, |ui, _path| {
                // 搜索框 = 玻璃输入框（悬停提亮），跟设置页/开始页同一套玻璃语言
                let hp = if ui.rect_to_global(sr).contains(mouse) { 1. } else { 0. };
                newui::glass_plate_w(
                    ui,
                    sr,
                    m.u(12.),
                    th,
                    hp,
                    Color::new(0., 0., 0., 0.),
                    newui::glass_border(th, hp),
                );
            });
            newui::draw_icon(
                ui,
                m,
                Icon::Search,
                Rect::new(
                    sr.x + m.u(13. * lib_scale),
                    sr.center().y - m.u(icon_base / 2.),
                    m.u(icon_base),
                    m.u(icon_base),
                ),
                th.text_mute,
                1.7,
            );
            let tx = sr.x + m.u(37. * lib_scale);
            if self.search_str.is_empty() {
                newui::text(ui, m, "搜索曲名或曲师…", tx, sr.center().y, icon_base, th.text_mute, (0., 0.5));
            } else {
                newui::text_ellipsis(
                    ui,
                    m,
                    self.search_str.clone(),
                    tx,
                    sr.center().y,
                    icon_base,
                    sr.right() - m.u(40. * lib_scale) - tx,
                    th.text_hi,
                    (0., 0.5),
                );
                let cr = Rect::new(
                    sr.right() - m.u(32. * lib_scale),
                    sr.center().y - m.u(icon_base * 0.55),
                    m.u(icon_base * 1.1),
                    m.u(icon_base * 1.1),
                );
                self.search_clr_btn.set(ui, cr);
                newui::draw_icon(ui, m, Icon::X, cr.feather(-cr.w * 0.3), th.text_dim, 1.8);
            }
        }

        // ---- 排序 chips + 正倒序（设计稿 `.sort-group`；窄屏 / 空间不足隐藏，排序走操作菜单） ----
        if show_chips {
            const ORDER_LABELS: [&str; 4] = ["默认", "曲名", "难度", "评分"];
            let mut cx = sr.right() + m.u(12.);
            for i in 0..4 {
                let label = ORDER_LABELS[i];
                let w = newui::measure(ui, m, label.to_owned(), 11.).w + m.u(26.);
                let r = Rect::new(cx, tool_row.center().y - m.u(18.), w, m.u(36.));
                let on = (i == 0 && self.current_order == ChartOrder::Default)
                    || (i == 1 && self.current_order == ChartOrder::Name)
                    || (i == 2 && self.current_order == ChartOrder::Difficulty)
                    || (i == 3 && self.current_order == ChartOrder::Rating);
                self.sort_chips[i].build(ui, t, r, |ui, path| {
                    if on {
                        // 选中 chip：强调色渐变（浅色主题下也是深蓝）→ 文字用纯白
                        ui.fill_path(&path, newui::hgrad(r, th.accent, th.glow));
                    } else {
                        // 未选中 chip = 玻璃（悬停提亮 + 1px 浅边）
                        let hp = if ui.rect_to_global(r).contains(mouse) { 1. } else { 0. };
                        newui::glass_plate_w(ui, r, m.u(10.), th, hp, Color::new(0., 0., 0., 0.), newui::glass_border(th, hp));
                    }
                });
                newui::text_center(
                    ui,
                    m,
                    label,
                    r.center().x,
                    r.center().y,
                    11.,
                    if on { Color::new(1., 1., 1., 1.) } else { th.text_mute },
                );
                cx = r.right() + m.u(4.);
            }
            let r = Rect::new(cx, tool_row.center().y - m.u(18.), m.u(40.), m.u(36.));
            self.sort_rev_btn.build(ui, t, r, |ui, _path| {
                let hp = if ui.rect_to_global(r).contains(mouse) { 1. } else { 0. };
                newui::glass_plate_w(ui, r, m.u(10.), th, hp, Color::new(0., 0., 0., 0.), newui::glass_border(th, hp));
                if self.order_rev {
                    newui::rounded_stroke(ui, r, m.u(10.), m.u(1.), th.with_a(th.glow, 0.5));
                }
            });
            newui::draw_icon(ui, m, Icon::Sort, r.feather(-r.w * 0.32), if self.order_rev { th.accent_hi } else { th.text_dim }, 1.7);
        } else {
            self.sort_chips.iter_mut().for_each(|b| b.invalidate());
            self.sort_rev_btn.invalidate();
        }

        // ---- 右侧操作按钮（横屏在顶栏上，竖屏在自己的那一行） ----
        {
            let mut rx = icons_row.right() - m.u(6.);
            let icon_btn = |ui: &mut Ui, rx: &mut f32, btn: &mut DRectButton, icon: Icon, active: bool, fill: Color, key: Option<&'static str>| {
                // 尺寸随顶部三行一起放大（与 heads[1] 的高度 m.u(46.*lib_scale) 对齐）
                let side = m.u(46. * lib_scale);
                let mut r = Rect::new(*rx - side, icons_row.center().y - side / 2., side, side);
                // HUD 自定义布局：单个图标按钮
                if let Some(k) = key {
                    r = crate::hud::slot_or(ui, "library", k, crate::hud::Cap(true, true, true), r);
                }
                btn.build(ui, t, r, |ui, _path| {
                    // 图标按钮 = 玻璃（`fill` 当作强调色 tint：中性态传透明即可）
                    let hp = if ui.rect_to_global(r).contains(mouse) { 1. } else { 0. };
                    newui::glass_plate_w(ui, r, m.u(12.), th, hp, fill, newui::glass_border(th, hp));
                    if active {
                        newui::rounded_stroke(ui, r, m.u(12.), m.u(1.), th.with_a(th.glow, 0.45));
                    }
                });
                newui::draw_icon(ui, m, icon, r.feather(-r.w * 0.3), if active { th.accent_hi } else { th.text_dim }, 1.7);
                *rx = r.x - m.u(6.);
                r
            };

            if multi_select {
                // 多选：取消 / 已选 N / 操作
                let order_r = icon_btn(ui, &mut rx, &mut self.multi_operation_btn, Icon::Menu, true, th.accent, None);
                if self.need_show_multi_operation_menu {
                    self.need_show_multi_operation_menu = false;
                    self.multi_operation_menu
                        .set_auto_adjust(Some(ui.screen_rect().nonuniform_feather(-0.03, -0.05)));
                    self.multi_operation_menu.set_bottom(true);
                    self.multi_operation_menu.set_selected(usize::MAX);
                    self.multi_operation_menu.set_auto_dismiss(false);
                    self.multi_operation_menu.show(ui, t, Rect::new(order_r.x, order_r.bottom() + m.u(4.), 0.35, 0.4));
                }

                let selected = self.tabs.selected().view.multi_select.as_ref().map_or(0, |v| v.len());
                let text = tl!("multi-select-status", "count" => selected);
                let tw = newui::measure(ui, m, text.clone(), 13.).w;
                let r = Rect::new(rx - tw - m.u(52.), icons_row.center().y - m.u(23.), tw + m.u(52.), m.u(46.));
                self.multi_select_btn.build(ui, t, r, |ui, _path| {
                    let hp = if ui.rect_to_global(r).contains(mouse) { 1. } else { 0. };
                    newui::glass_plate_w(
                        ui,
                        r,
                        m.u(12.),
                        th,
                        hp,
                        th.with_a(th.accent, 0.16),
                        newui::glass_border(th, hp),
                    );
                    newui::rounded_stroke(ui, r, m.u(12.), m.u(1.), th.with_a(th.glow, 0.4));
                });
                newui::draw_icon(ui, m, Icon::Edit, Rect::new(r.x + m.u(13.), r.center().y - m.u(9.5), m.u(19.), m.u(19.)), th.accent_hi, 1.7);
                newui::text(ui, m, text, r.x + m.u(38.), r.center().y, 14., th.text_hi, (0., 0.5));
                rx = r.x - m.u(6.);
                if self.need_show_multi_select_menu {
                    self.need_show_multi_select_menu = false;
                    self.multi_select_menu
                        .set_auto_adjust(Some(ui.screen_rect().nonuniform_feather(-0.03, -0.05)));
                    self.multi_select_menu.set_bottom(true);
                    self.multi_select_menu.set_selected(usize::MAX);
                    self.multi_select_menu.show(ui, t, Rect::new(r.x, r.bottom() + m.u(4.), 0.3, 0.2));
                }

                let _ = icon_btn(ui, &mut rx, &mut self.multi_select_cancel_btn, Icon::X, false, th.with_a(th.red, 0.14), None);
                return;
            }

            let order_r = icon_btn(ui, &mut rx, &mut self.order_btn, Icon::Sort, false, th.wash(0.02), Some("order"));
            if self.need_show_order_meta_menu {
                self.need_show_order_meta_menu = false;
                self.order_meta_menu
                    .set_auto_adjust(Some(ui.screen_rect().nonuniform_feather(-0.03, -0.05)));
                if is_local {
                    self.order_menu_options = vec![ChartOrder::Default, ChartOrder::Name, ChartOrder::Difficulty];
                } else {
                    self.order_menu_options = vec![ChartOrder::Default, ChartOrder::Rating, ChartOrder::Name, ChartOrder::Difficulty];
                }
                self.order_meta_menu.set_bottom(true);
                self.order_meta_menu.set_auto_dismiss(false);
                self.update_order_meta_menu_options();
                self.order_meta_menu.set_selected(usize::MAX);
                self.order_meta_menu.show(ui, t, Rect::new(order_r.x, order_r.bottom() + m.u(4.), 0.35, 0.2));
            }
            if is_local {
                let active = self.current_fav_index.is_some();
                let _ = icon_btn(ui, &mut rx, &mut self.fav_btn, Icon::Star, active, if active { th.with_a(th.accent, 0.2) } else { th.wash(0.02) }, Some("fav"));
                if !multi_select {
                    let _ = icon_btn(ui, &mut rx, &mut self.import_btn, Icon::Plus, false, th.wash(0.02), Some("import"));
                } else {
                    self.import_btn.invalidate();
                }
            } else {
                let _ = icon_btn(ui, &mut rx, &mut self.filter_btn, Icon::Filter, false, th.wash(0.02), Some("filter"));
            }

            // 课题模式（T7.3）+ 自定义 RKS（T7.4）+ 最佳谱面榜（T7.2）：
            // 工具行最左侧的三个文字入口
            if show_entries {
                let h = m.u(46.);
                let cw = m.u(160.);
                let rw = m.u(120.);
                let bw = m.u(110.);
                let challenge_r = Rect::new(rx - cw, icons_row.center().y - h / 2., cw, h);
                let rks_r = Rect::new(challenge_r.x - m.u(6.) - rw, challenge_r.y, rw, h);
                let board_r = Rect::new(rks_r.x - m.u(6.) - bw, challenge_r.y, bw, h);
                // 边界：横屏以**搜索框右缘**为界（同一行，不能压到搜索栏）；
                // 竖屏入口在**独立的图标行**上，以该行左缘为界即可。
                let left_limit = if compact { icons_row.x } else { sr.right() } + m.u(12.);
                if board_r.x > left_limit {
                    let n = self.rks.best_board_count();
                    self.best_board.render_entry(ui, t, board_r, n);
                    self.rks.render_entry(ui, t, m, rks_r);
                    self.challenge.render_entry(ui, t, challenge_r);
                } else {
                    self.best_board.hide_entry();
                    self.rks.hide_entry();
                    self.challenge.hide_entry();
                }
            } else {
                self.best_board.hide_entry();
                self.rks.hide_entry();
                self.challenge.hide_entry();
            }
        }

        let _ = chosen;
    }

    /// 底部翻页条（上架 / 特殊 / 未上架 / 热门）。
    fn render_pagination(&mut self, ui: &mut Ui, t: f32, m: &Metrics, th: &newui::Theme, grid: Rect) {
        let total_page = self.total_page();
        if total_page <= 0 {
            return;
        }
        let cy = grid.bottom() + m.u(14.);
        let cx = grid.center().x;
        let (mx, my) = newui::mouse_world();
        let mouse = vec2(mx, my);

        let chip = |ui: &mut Ui, btn: &mut DRectButton, r: Rect, icon: Icon, enabled: bool| {
            btn.build(ui, t, r, |ui, _path| {
                // 翻页按钮 = 玻璃圆钮（禁用时整块压暗，不再是死板灰块）
                let hp = if ui.rect_to_global(r).contains(mouse) && enabled { 1. } else { 0. };
                newui::glass_plate_w(
                    ui,
                    r,
                    m.u(10.),
                    th,
                    hp,
                    if enabled {
                        Color::new(0., 0., 0., 0.)
                    } else {
                        th.wash(0.05)
                    },
                    newui::glass_border(th, hp),
                );
            });
            newui::draw_icon(
                ui,
                m,
                icon,
                r.feather(-r.w * 0.34),
                if enabled { th.text_dim } else { th.text_mute },
                1.7,
            );
        };
        let pr = Rect::new(cx - m.u(150.), cy - m.u(15.), m.u(34.), m.u(30.));
        // HUD 自定义布局：页码指示居中块 + 上一页 / 下一页按钮（只可水平移动）
        let pager = crate::hud::slot_or(
            ui,
            "library",
            "pager",
            crate::hud::Cap(true, false, false),
            Rect::new(cx - m.u(150.), cy - m.u(15.), m.u(300.), m.u(30.)),
        );
        let pr = crate::hud::slot_or(ui, "library", "prev-page", crate::hud::Cap(true, false, false), pr);
        chip(
            ui,
            &mut self.prev_page_btn,
            pr,
            Icon::ChevronLeft,
            self.current_page != 0,
        );
        let nr = Rect::new(cx + m.u(116.), cy - m.u(15.), m.u(34.), m.u(30.));
        let nr = crate::hud::slot_or(ui, "library", "next-page", crate::hud::Cap(true, false, false), nr);
        chip(
            ui,
            &mut self.next_page_btn,
            nr,
            Icon::ChevronRight,
            self.current_page + 1 < total_page,
        );
        newui::text_center(
            ui,
            m,
            format!("{} / {}", self.current_page + 1, total_page),
            pager.center().x,
            cy,
            12.,
            th.text_dim,
        );
    }
}

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::new(r as f32 / 255., g as f32 / 255., b as f32 / 255., 1.)
}
