use std::{borrow::Cow, collections::HashMap, ops::RangeInclusive, os::raw::c_void, panic::{self, AssertUnwindSafe}, sync::{atomic::{self, AtomicBool}, Arc, Mutex}, thread, time::Instant};

use egui_scale::EguiScale;
use fnv::{FnvHashSet, FnvHashMap};
use once_cell::sync::{Lazy, OnceCell};
use rust_i18n::t;
use chrono::{Utc, Datelike};

use crate::il2cpp::{
    ext::{StringExt, Il2CppStringExt},
    hook::{
        umamusume::{CameraData::ShadowResolution, CySpringController::SpringUpdateMode, GameSystem, GraphicSettings::{GraphicsQuality, MsaaQuality}, Localize, TimeUtil::BgSeason},
        UnityEngine_CoreModule::{Application, Texture::AnisoLevel}
    },
    symbols::Thread,
    types::Il2CppObject
};

#[cfg(target_os = "android")]
use crate::il2cpp::{
    hook::{umamusume::WebViewManager, UnityEngine_CoreModule::{TouchScreenKeyboard, TouchScreenKeyboardType}},
    symbols::GCHandle,
    types::{Il2CppString, RangeInt}
};

#[cfg(target_os = "windows")]
use crate::il2cpp::hook::UnityEngine_CoreModule::QualitySettings;

use super::{
    hachimi::{self, Language, REPO_PATH, WEBSITE_URL},
    http::AsyncRequest,
    tl_repo::{self, RepoInfo},
    utils,
    Hachimi
};

macro_rules! add_font {
    ($fonts:expr, $family_fonts:expr, $filename:literal) => {
        $fonts.font_data.insert(
            $filename.to_owned(),
            egui::FontData::from_static(include_bytes!(concat!("../../assets/fonts/", $filename))).into()
        );
        $family_fonts.push($filename.to_owned());
    };
}

use egui::Color32;

static UNITY_COLORS: std::sync::OnceLock<FnvHashMap<&'static str, Color32>> = std::sync::OnceLock::new();
pub fn get_unity_colors() -> &'static FnvHashMap<&'static str, Color32> {
    UNITY_COLORS.get_or_init(|| {
        let mut m = FnvHashMap::with_capacity_and_hasher(22, Default::default());
        m.insert("aqua", Color32::from_rgb(0, 255, 255));
        m.insert("cyan", Color32::from_rgb(0, 255, 255));
        m.insert("black", Color32::from_rgb(0, 0, 0));
        m.insert("blue", Color32::from_rgb(0, 0, 255));
        m.insert("brown", Color32::from_rgb(165, 42, 42));
        m.insert("darkblue", Color32::from_rgb(0, 0, 160));
        m.insert("fuchsia", Color32::from_rgb(255, 0, 255));
        m.insert("magenta", Color32::from_rgb(255, 0, 255));
        m.insert("green", Color32::from_rgb(0, 128, 0));
        m.insert("grey", Color32::from_rgb(128, 128, 128));
        m.insert("lightblue", Color32::from_rgb(173, 216, 230));
        m.insert("lime", Color32::from_rgb(0, 255, 0));
        m.insert("maroon", Color32::from_rgb(128, 0, 0));
        m.insert("navy", Color32::from_rgb(0, 0, 128));
        m.insert("olive", Color32::from_rgb(128, 128, 0));
        m.insert("orange", Color32::from_rgb(255, 165, 0));
        m.insert("purple", Color32::from_rgb(128, 0, 128));
        m.insert("red", Color32::from_rgb(255, 0, 0));
        m.insert("silver", Color32::from_rgb(192, 192, 192));
        m.insert("teal", Color32::from_rgb(0, 128, 128));
        m.insert("white", Color32::from_rgb(255, 255, 255));
        m.insert("yellow", Color32::from_rgb(255, 255, 0));
        m
    })
}

type BoxedWindow = Box<dyn Window + Send + Sync>;
pub struct Gui {
    pub context: egui::Context,
    pub input: egui::RawInput,
    default_style: egui::Style,
    pub gui_scale: f32,

    pub finalized_scale: f32,
    pub start_time: Instant,
    pub prev_main_axis_size: i32,
    last_fps_update: Instant,
    tmp_frame_count: u32,
    fps_text: String,

    show_menu: bool,

    splash_visible: bool,
    splash_tween: TweenInOutWithDelay,
    splash_sub_str: String,

    menu_visible: bool,
    menu_anim_time: Option<Instant>,
    menu_fps_value: i32,

    #[cfg(target_os = "windows")]
    menu_vsync_value: i32,

    pub update_progress_visible: bool,

    notifications: Vec<Notification>,
    windows: Vec<BoxedWindow>
}

const PIXELS_PER_POINT_RATIO: f32 = 3.0/1080.0;
const BACKGROUND_COLOR: egui::Color32 = egui::Color32::from_rgba_premultiplied(27, 27, 27, 220);
const TEXT_COLOR: egui::Color32 = egui::Color32::from_gray(170);

static INSTANCE: OnceCell<Mutex<Gui>> = OnceCell::new();
static IS_CONSUMING_INPUT: AtomicBool = AtomicBool::new(false);
static mut DISABLED_GAME_UIS: once_cell::unsync::Lazy<FnvHashSet<*mut Il2CppObject>> =
    once_cell::unsync::Lazy::new(|| FnvHashSet::default());
static PLUGIN_MENU_ITEMS: Lazy<Mutex<Vec<PluginMenuItem>>> = Lazy::new(|| Mutex::new(Vec::new()));
static PLUGIN_MENU_SECTIONS: Lazy<Mutex<Vec<PluginMenuSection>>> = Lazy::new(|| Mutex::new(Vec::new()));
static PLUGIN_MENU_ICONS: Lazy<Mutex<HashMap<String, PluginMenuIcon>>> = Lazy::new(|| Mutex::new(HashMap::new()));
static PLUGIN_NOTIFICATIONS: Lazy<Mutex<Vec<String>>> = Lazy::new(|| Mutex::new(Vec::new()));

pub type PluginMenuCallback = extern "C" fn(userdata: *mut c_void);
pub type PluginMenuSectionCallback = extern "C" fn(ui: *mut c_void, userdata: *mut c_void);

#[derive(Clone)]
struct PluginMenuItem {
    label: String,
    callback: Option<PluginMenuCallback>,
    userdata: usize
}

#[derive(Clone)]
struct PluginMenuIcon {
    uri: String,
    bytes: Arc<[u8]>,
}

#[derive(Clone)]
struct PluginMenuSection {
    title: Option<String>,
    icon: Option<PluginMenuIcon>,
    callback: PluginMenuSectionCallback,
    userdata: usize
}

pub fn register_plugin_menu_item(label: String, callback: Option<PluginMenuCallback>, userdata: *mut c_void) {
    PLUGIN_MENU_ITEMS.lock().unwrap().push(PluginMenuItem {
        label,
        callback,
        userdata: userdata as usize
    });
}

pub fn register_plugin_menu_section(callback: PluginMenuSectionCallback, userdata: *mut c_void) {
    PLUGIN_MENU_SECTIONS.lock().unwrap().push(PluginMenuSection {
        title: None,
        icon: None,
        callback,
        userdata: userdata as usize
    });
}

pub fn register_plugin_menu_section_with_icon(
    title: String,
    uri: String,
    bytes: Vec<u8>,
    callback: PluginMenuSectionCallback,
    userdata: *mut c_void
) -> bool {
    if title.is_empty() || uri.is_empty() || bytes.is_empty() {
        return false;
    }
    PLUGIN_MENU_SECTIONS.lock().unwrap().push(PluginMenuSection {
        title: Some(title),
        icon: Some(PluginMenuIcon { uri, bytes: bytes.into() }),
        callback,
        userdata: userdata as usize
    });
    true
}

pub fn register_plugin_menu_icon(label: String, uri: String, bytes: Vec<u8>) -> bool {
    if label.is_empty() || uri.is_empty() || bytes.is_empty() {
        return false;
    }
    PLUGIN_MENU_ICONS.lock().unwrap().insert(label, PluginMenuIcon {
        uri,
        bytes: bytes.into(),
    });
    true
}

pub fn enqueue_plugin_notification(message: String) {
    PLUGIN_NOTIFICATIONS.lock().unwrap().push(message);
}

fn get_plugin_menu_items() -> Vec<PluginMenuItem> {
    PLUGIN_MENU_ITEMS.lock().unwrap().clone()
}

fn get_plugin_menu_sections() -> Vec<PluginMenuSection> {
    PLUGIN_MENU_SECTIONS.lock().unwrap().clone()
}

fn get_plugin_menu_icon(label: &str) -> Option<PluginMenuIcon> {
    PLUGIN_MENU_ICONS.lock().unwrap().get(label).cloned()
}

fn drain_plugin_notifications() -> Vec<String> {
    let mut notifications = PLUGIN_NOTIFICATIONS.lock().unwrap();
    std::mem::take(&mut *notifications)
}

use std::sync::atomic::Ordering;

#[cfg(target_os = "android")]
use std::sync::atomic::{AtomicI32, AtomicPtr};
#[cfg(target_os = "android")]
static PENDING_KB_TYPE: AtomicI32 = AtomicI32::new(0);
#[cfg(target_os = "android")]
static PENDING_KEYBOARD_TEXT: AtomicPtr<Il2CppString> = AtomicPtr::new(std::ptr::null_mut());
#[cfg(target_os = "android")]
static ACTIVE_KEYBOARD: AtomicPtr<Il2CppObject> = AtomicPtr::new(std::ptr::null_mut());
#[cfg(target_os = "android")]
pub static KEYBOARD_GC_HANDLE: Lazy<Mutex<Option<GCHandle>>> = Lazy::new(|| Mutex::default());
#[cfg(target_os = "android")]
static KEYBOARD_SELECTION: std::sync::LazyLock<Mutex<RangeInt>> = std::sync::LazyLock::new(|| {
    Mutex::new(RangeInt::new(0, 1))
});

fn get_scale_salt(ctx: &egui::Context) -> f32 {
    ctx.data(|d| d.get_temp::<f32>(egui::Id::new("gui_scale_salt"))).unwrap_or(1.0)
}

fn get_scale(ctx: &egui::Context) -> f32 {
    ctx.data(|d| d.get_temp::<f32>(egui::Id::new("gui_scale"))).unwrap_or(1.0)
}

#[cfg(target_os = "android")]
fn is_ime_visible() -> bool {
    let kb_ptr = ACTIVE_KEYBOARD.load(Ordering::Relaxed);
    if kb_ptr.is_null() {
        return false;
    }
    TouchScreenKeyboard::get_status(kb_ptr) == TouchScreenKeyboard::Status::Visible
}

#[cfg(target_os = "android")]
fn ime_scroll_padding(ctx: &egui::Context) -> f32 {
    if !is_ime_visible() {
        return 0.0;
    }
    ctx.input(|i| i.viewport_rect().height() * 0.35)
}

#[cfg(target_os = "android")]
pub fn handle_android_keyboard<T: 'static>(res: &egui::Response, val: &mut T) {
    if res.lost_focus() {
        let kb_ptr = ACTIVE_KEYBOARD.load(Ordering::Relaxed);
        if !kb_ptr.is_null() {
            TouchScreenKeyboard::set_active(kb_ptr, false);
            ACTIVE_KEYBOARD.store(std::ptr::null_mut(), Ordering::Relaxed);
            *KEYBOARD_GC_HANDLE.lock().unwrap() = None;
        }
        return;
    }

    if !res.has_focus() {
        return;
    }

    use utils::{char_to_utf16_index, utf16_to_char_index};
    use egui::{text::{CCursor, CCursorRange}, widgets::text_edit::TextEditState};

    let val_any = val as &dyn std::any::Any;
    PENDING_KB_TYPE.store(TouchScreenKeyboardType::KeyboardType::Default as i32, Ordering::Relaxed);

    let text = if let Some(s) = val_any.downcast_ref::<String>() {
        s.clone()
    } else if let Some(f) = val_any.downcast_ref::<f32>() {
        PENDING_KB_TYPE.store(TouchScreenKeyboardType::KeyboardType::DecimalPad as i32, Ordering::Relaxed);
        if f.fract() == 0.0 { format!("{:.1}", f) } else { f.to_string() }
    } else if let Some(i) = val_any.downcast_ref::<i32>() {
        PENDING_KB_TYPE.store(TouchScreenKeyboardType::KeyboardType::NumberPad as i32, Ordering::Relaxed);
        i.to_string()
    } else {
        String::new() 
    };
    
    if res.gained_focus() {
        res.scroll_to_me(Some(egui::Align::Center));

        let ptr = text.to_il2cpp_string();
        PENDING_KEYBOARD_TEXT.store(ptr, Ordering::Relaxed);

        let initial_selection = res.ctx.data(|data| {
            data.get_temp::<TextEditState>(res.id)
            .and_then(|state| state.cursor.char_range())
            .map(|range| {
                let start_char = range.primary.index.min(range.secondary.index);
                let end_char = range.primary.index.max(range.secondary.index);

                let start_u16 = char_to_utf16_index(&text, start_char);
                let end_u16 = char_to_utf16_index(&text, end_char);

                RangeInt::new(start_u16, end_u16 - start_u16)
            })
            .unwrap_or(RangeInt::new(char_to_utf16_index(&text, text.chars().count()), 0))
        });
        *KEYBOARD_SELECTION.lock().unwrap() = initial_selection;

        Thread::main_thread().schedule(|| {
            let ptr = PENDING_KEYBOARD_TEXT.swap(std::ptr::null_mut(), Ordering::Relaxed);
            let typ: TouchScreenKeyboardType::KeyboardType = unsafe { *(&PENDING_KB_TYPE.load(Ordering::Relaxed) as *const i32 as *const TouchScreenKeyboardType::KeyboardType) };

            if !ptr.is_null() {
                let keyboard = TouchScreenKeyboard::Open(ptr, typ, false, false, false);
                TouchScreenKeyboard::set_selection(keyboard, *KEYBOARD_SELECTION.lock().unwrap());
                let handle = GCHandle::new(keyboard, false);
                *KEYBOARD_GC_HANDLE.lock().unwrap() = Some(handle);
                ACTIVE_KEYBOARD.store(keyboard, Ordering::Relaxed);
            }
        });
    }

    let kb_ptr = ACTIVE_KEYBOARD.load(Ordering::Relaxed);
    if !kb_ptr.is_null() {
        let status = TouchScreenKeyboard::get_status(kb_ptr);

        if status == TouchScreenKeyboard::Status::Visible {
            let unity_range = TouchScreenKeyboard::get_selection(kb_ptr);

            let kb_txt_ptr = TouchScreenKeyboard::get_text(kb_ptr);
            if let Some(kb_ref) = unsafe { kb_txt_ptr.as_ref() } {
                let kb_txt_str = kb_ref.as_utf16str().to_string();

                let val_any_mut = val as &mut dyn std::any::Any;

                if let Some(s) = val_any_mut.downcast_mut::<String>() {
                    if *s != kb_txt_str { *s = kb_txt_str.clone(); }
                } else if let Some(f) = val_any_mut.downcast_mut::<f32>() {
                    if let Ok(parsed) = kb_txt_str.parse::<f32>() {
                        let changed = !egui::emath::almost_equal(*f, parsed, 1e-6);
                        let drafting = kb_txt_str.ends_with('.') || (kb_txt_str.contains('.') && kb_txt_str.ends_with('0'));

                        if changed && !drafting {
                            *f = parsed;
                        }
                    }
                } else if let Some(i) = val_any_mut.downcast_mut::<i32>() {
                    if let Ok(parsed) = kb_txt_str.parse::<i32>() { 
                        if *i != parsed { *i = parsed; }
                    }
                }

                let kb_txt_clone = kb_txt_str.clone(); 
                res.ctx.data_mut(|data| {
                    if let Some(mut state) = data.get_temp::<TextEditState>(res.id) {
                        let start_char = utf16_to_char_index(&kb_txt_clone, unity_range.start as usize);
                        let end_char = utf16_to_char_index(&kb_txt_clone, (unity_range.start + unity_range.length) as usize);

                        let new_range = CCursorRange::two(CCursor::new(start_char), CCursor::new(end_char));

                        if state.cursor.char_range() != Some(new_range) {
                            state.cursor.set_char_range(Some(new_range));
                            data.insert_temp(res.id, state);
                        }
                    }
                });
            }
            res.ctx.request_repaint();
        }

        if status != TouchScreenKeyboard::Status::Visible {
            res.surrender_focus();
            res.ctx.memory_mut(|mem| mem.stop_text_input());
            res.ctx.data_mut(|data| {
                data.remove::<egui::widgets::text_edit::TextEditState>(res.id);
            });

            ACTIVE_KEYBOARD.store(std::ptr::null_mut(), Ordering::Relaxed);
            *KEYBOARD_GC_HANDLE.lock().unwrap() = None;
            res.ctx.request_repaint();
        }
    }
}

impl Gui {
    // Call this from the render thread!
    pub fn instance_or_init(
        #[cfg_attr(target_os = "windows", allow(unused))] open_key_id: &str
    ) -> &Mutex<Gui> {
        if let Some(instance) = INSTANCE.get() {
            return instance;
        }

        let hachimi = Hachimi::instance();
        let config = hachimi.config.load();

        let context = egui::Context::default();
        egui_extras::install_image_loaders(&context);

        context.set_fonts(Self::get_font_definitions());

        let mut style = egui::Style::default();
        style.spacing.button_padding = egui::Vec2::new(8.0, 5.0);
        style.interaction.selectable_labels = false;

        context.set_style(style);

        let mut visuals = egui::Visuals::dark();
        visuals.panel_fill = BACKGROUND_COLOR;
        visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0, TEXT_COLOR);
        context.set_visuals(visuals);

        let default_style = context.style().as_ref().clone();

        let mut fps_value = hachimi.target_fps.load(atomic::Ordering::Relaxed);
        if fps_value == -1 {
            fps_value = 30;
        }

        let mut windows: Vec<BoxedWindow> = Vec::new();
        if !config.skip_first_time_setup {
            windows.push(Box::new(FirstTimeSetupWindow::new()));
        }

        let now = Instant::now();
        let instance = Gui {
            context,
            input: egui::RawInput::default(),
            gui_scale: 1.0,
            finalized_scale: 1.0,
            default_style,
            start_time: now,
            prev_main_axis_size: 1,
            last_fps_update: now,
            tmp_frame_count: 0,
            fps_text: "FPS: 0".to_string(),

            show_menu: false,

            splash_visible: true,
            splash_tween: TweenInOutWithDelay::new(0.8, 3.0, Easing::OutQuad),
            splash_sub_str: {
                #[cfg(target_os = "windows")]
                {
                    let key_label = crate::windows::utils::vk_to_display_label(hachimi.config.load().windows.menu_open_key);
                    t!("splash_sub", open_key_str = key_label).into_owned()
                }
                #[cfg(not(target_os = "windows"))]
                {
                    t!("splash_sub", open_key_str = t!(open_key_id)).into_owned()
                }
            },

            menu_visible: false,
            menu_anim_time: None,
            menu_fps_value: fps_value,

            #[cfg(target_os = "windows")]
            menu_vsync_value: hachimi.vsync_count.load(atomic::Ordering::Relaxed),

            update_progress_visible: false,

            notifications: Vec::new(),
            windows
        };
        unsafe {
            INSTANCE.set(Mutex::new(instance)).unwrap_unchecked();

            // Doing auto update check here to ensure that the updater can access the gui
            hachimi.run_auto_update_check();

            INSTANCE.get().unwrap_unchecked()
        }
    }

    pub fn instance() -> Option<&'static Mutex<Gui>> {
        INSTANCE.get()
    }

    fn get_font_definitions() -> egui::FontDefinitions {
        let mut fonts = egui::FontDefinitions::default();
        let proportional_fonts = fonts.families.get_mut(&egui::FontFamily::Proportional).unwrap();

        add_font!(fonts, proportional_fonts, "AlibabaPuHuiTi-3-45-Light.otf");
        add_font!(fonts, proportional_fonts, "NotoSans-Regular.ttf");
        add_font!(fonts, proportional_fonts, "NotoSansJP-Regular.ttf");
        add_font!(fonts, proportional_fonts, "FontAwesome.otf");

        fonts
    }

    pub fn set_screen_size(&mut self, width: i32, height: i32) {
        let main_axis_size = if width < height { width } else { height };
        let pixels_per_point = main_axis_size as f32 * PIXELS_PER_POINT_RATIO;
        self.context.set_pixels_per_point(pixels_per_point);

        self.input.screen_rect = Some(egui::Rect {
            min: egui::Pos2::default(),
            max: egui::Pos2::new(
                width as f32 / self.context.pixels_per_point(),
                height as f32 / self.context.pixels_per_point()
            )
        });

        self.prev_main_axis_size = main_axis_size;
    }

    fn take_input(&mut self) -> egui::RawInput {
        self.input.time = Some(self.start_time.elapsed().as_secs_f64());
        self.input.take()
    }

    fn update_fps(&mut self) {
        let delta = self.last_fps_update.elapsed().as_secs_f64();
        if delta > 0.5 {
            let fps = (self.tmp_frame_count as f64 * (0.5 / delta) * 2.0).round();
            self.fps_text = t!("menu.fps_text", fps = fps).into_owned();
            self.tmp_frame_count = 1;
            self.last_fps_update = Instant::now();
        }
        else {
            self.tmp_frame_count += 1;
        }
    }

    pub fn run(&mut self) -> egui::FullOutput {
        self.update_fps();
        let input = self.take_input();

        let live_scale = Hachimi::instance().config.load().gui_scale;
        self.gui_scale = live_scale;

        if !self.context.is_using_pointer() {
            self.finalized_scale = live_scale;
        }

        self.context.data_mut(|d| {
            d.insert_temp(egui::Id::new("gui_scale"), live_scale);
            d.insert_temp(egui::Id::new("gui_scale_salt"), self.finalized_scale);
        });

        let mut style = self.default_style.clone();
        if live_scale != 1.0 {
            style.scale(live_scale);
        }
        self.context.set_style(style);

        self.context.begin_pass(input);
        
        if self.menu_visible { self.run_menu(); }
        if self.update_progress_visible { self.run_update_progress(); }

        self.run_windows();
        self.run_notifications();

        if self.splash_visible { self.run_splash(); }
        if hachimi::CONFIG_LOAD_ERROR.swap(false, Ordering::Relaxed) {
            self.show_notification(&t!("notification.config_error"));
        }

        // Store this as an atomic value so the input thread can check it without locking the gui
        IS_CONSUMING_INPUT.store(self.is_consuming_input(), atomic::Ordering::Relaxed);

        self.context.end_pass()
    }

    const ICON_IMAGE: egui::ImageSource<'static> = egui::include_image!("../../assets/icon.png");
    fn icon<'a>(ctx: &egui::Context) -> egui::Image<'a> {
        let scale = get_scale(ctx);
        egui::Image::new(Self::ICON_IMAGE)
            .fit_to_exact_size(egui::Vec2::new(24.0 * scale, 24.0 * scale))
    }

    fn icon_2x<'a>(ctx: &egui::Context) -> egui::Image<'a> {
        let scale = get_scale(ctx);
        egui::Image::new(Self::ICON_IMAGE)
            .fit_to_exact_size(egui::Vec2::new(48.0 * scale, 48.0 * scale))
    }

    fn run_splash(&mut self) {
        let ctx = &self.context;
        let scale = get_scale(ctx);

        let id = egui::Id::from("splash");
        let Some(tween_val) = self.splash_tween.run(ctx, id.with("tween")) else {
            self.splash_visible = false;
            return;
        };

        egui::Area::new(id)
        .fixed_pos(egui::Pos2 {
            x: (-250.0 * scale) * (1.0 - tween_val),
            y: 16.0 * scale
        })
        .show(ctx, |ui| {
            egui::Frame::NONE
            .fill(BACKGROUND_COLOR)
            .inner_margin(egui::Margin::same((10.0 * scale) as i8))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.add(Self::icon(ctx));
                    ui.heading("Hachimi");
                    ui.label(env!("HACHIMI_DISPLAY_VERSION"));
                });
                ui.label(&self.splash_sub_str);
            });
        });
    }

    fn run_menu(&mut self) {
        let hachimi = Hachimi::instance();
        let localized_data = hachimi.localized_data.load();
        let localize_dict_count = localized_data.localize_dict.len().to_string();
        let hashed_dict_count = localized_data.hashed_dict.len().to_string();

        let mut show_notification: Option<Cow<'_, str>> = None;
        let mut show_window: Option<BoxedWindow> = None;
        {
            let ctx = &self.context;
            let scale = get_scale(ctx);
            let salt = self.finalized_scale;
            egui::SidePanel::left(egui::Id::new("hachimi_menu").with(salt.to_bits()))
                .min_width(96.0 * scale)
                .default_width(200.0 * scale)
                .show_animated(ctx, self.show_menu, |ui| {
                ui.with_layout(egui::Layout::top_down_justified(egui::Align::TOP), |ui| {
                    #[cfg(target_os = "windows")]
                    {
                        ui.horizontal(|ui| {
                            ui.add(Self::icon(ctx));
                            ui.heading(t!("hachimi"));
                            if ui.button(" \u{f29c} ").clicked() {
                                show_window = Some(Box::new(AboutWindow::new()));
                            }
                        });
                        ui.label(env!("HACHIMI_DISPLAY_VERSION"));
                        if ui.button(t!("menu.close_menu")).clicked() {
                            self.show_menu = false;
                            self.menu_anim_time = None;
                        }
                    }
                    // did this because android phones have a notch
                    #[cfg(target_os = "android")]
                    {
                        ui.horizontal(|ui| {
                            ui.add(Self::icon(ctx));
                            ui.heading(t!("hachimi"));
                        });
                        ui.label(env!("HACHIMI_DISPLAY_VERSION"));
                        ui.horizontal(|ui| {
                            if ui.button(t!("menu.close_menu")).clicked() {
                                self.show_menu = false;
                                self.menu_anim_time = None;
                            }
                            if ui.button(" \u{f29c} ").clicked() {
                                show_window = Some(Box::new(AboutWindow::new()));
                            }
                        });
                    }
                    if ui.button(t!("menu.check_for_updates")).clicked() {
                        Hachimi::instance().updater.clone().check_for_updates(|_| {});
                    }
                    ui.separator();

                    egui::ScrollArea::vertical().show(ui, |ui| {
                        ui.heading(t!("menu.stats_heading"));
                        ui.label(&self.fps_text);
                        ui.label(t!("menu.localize_dict_entries", count = localize_dict_count));
                        ui.label(t!("menu.hashed_dict_entries", count = hashed_dict_count));
                        ui.separator();

                        ui.heading(t!("menu.config_heading"));
                        if ui.button(t!("menu.open_config_editor")).clicked() {
                            show_window = Some(Box::new(ConfigEditor::new()));
                        }
                        if ui.button(t!("menu.reload_config")).clicked() {
                            hachimi.reload_config();
                            show_notification = Some(t!("notification.config_reloaded"));
                        }
                        if ui.button(t!("menu.open_first_time_setup")).clicked() {
                            show_window = Some(Box::new(FirstTimeSetupWindow::new()));
                        }
                        ui.separator();

                        ui.heading(t!("menu.graphics_heading"));
                        ui.horizontal(|ui| {
                            ui.label(t!("menu.fps_label"));
                            let res = ui.add(egui::Slider::new(&mut self.menu_fps_value, 30..=240));
                            #[cfg(target_os = "android")]
                            handle_android_keyboard(&res, &mut self.menu_fps_value);
                            if res.lost_focus() || res.drag_stopped() {
                                hachimi.target_fps.store(self.menu_fps_value, atomic::Ordering::Relaxed);
                                Thread::main_thread().schedule(|| {
                                    Application::set_targetFrameRate(30);
                                });
                            }
                        });
                        #[cfg(target_os = "windows")]
                        {
                            use crate::windows::{discord, utils::set_window_topmost, wnd_hook};

                            ui.horizontal(|ui| {
                                let prev_value = self.menu_vsync_value;

                                ui.label(t!("menu.vsync_label"));
                                Self::run_vsync_combo(ui, &mut self.menu_vsync_value);

                                if prev_value != self.menu_vsync_value {
                                    hachimi.vsync_count.store(self.menu_vsync_value, atomic::Ordering::Relaxed);
                                    Thread::main_thread().schedule(|| {
                                        QualitySettings::set_vSyncCount(1);
                                    });
                                }
                            });
                            ui.horizontal(|ui| {
                                let mut value = hachimi.window_always_on_top.load(atomic::Ordering::Relaxed);

                                ui.label(t!("menu.stay_on_top"));
                                if ui.checkbox(&mut value, "").changed() {
                                    hachimi.window_always_on_top.store(value, atomic::Ordering::Relaxed);
                                    Thread::main_thread().schedule(|| {
                                        let topmost = Hachimi::instance().window_always_on_top.load(atomic::Ordering::Relaxed);
                                        unsafe { _ = set_window_topmost(wnd_hook::get_target_hwnd(), topmost); }
                                    });
                                }
                            });
                            ui.horizontal(|ui| {
                                let mut value = hachimi.discord_rpc.load(atomic::Ordering::Relaxed);
                                
                                ui.label(t!("menu.discord_rpc"));
                                if ui.checkbox(&mut value, "").changed() {
                                    hachimi.discord_rpc.store(value, atomic::Ordering::Relaxed);
                                    if let Err(e) = if value { discord::start_rpc() } else { discord::stop_rpc() } {
                                        error!("{}", e);
                                    }
                                }
                            });
                        }
                        ui.separator();

                        ui.heading(t!("menu.translation_heading"));
                        if ui.button(t!("menu.reload_localized_data")).clicked() {
                            hachimi.load_localized_data();
                            show_notification = Some(t!("notification.localized_data_reloaded"));
                        }
                        if ui.button(t!("menu.tl_check_for_updates")).clicked() {
                            hachimi.tl_updater.clone().check_for_updates(false);
                        }
                        if ui.button(t!("menu.tl_check_for_updates_pedantic")).clicked() {
                            hachimi.tl_updater.clone().check_for_updates(true);
                        }
                        if hachimi.config.load().translator_mode {
                            if ui.button(t!("menu.dump_localize_dict")).clicked() {
                                Thread::main_thread().schedule(|| {
                                    let data = Localize::dump_strings();
                                    let dict_path = Hachimi::instance().get_data_path("localize_dump.json");
                                    let mut gui = Gui::instance().unwrap().lock().unwrap();
                                    if let Err(e) = utils::write_json_file(&data, dict_path) {
                                        gui.show_notification(&e.to_string())
                                    }
                                    else {
                                        gui.show_notification(&t!("notification.saved_localize_dump"))
                                    }
                                })
                            }
                        }
                        ui.separator();

                        let plugin_items = get_plugin_menu_items();
                        if !plugin_items.is_empty() {
                            ui.heading("Plugins");
                            for item in plugin_items {
                                let icon = get_plugin_menu_icon(&item.label);
                                let clicked = if let Some(icon) = icon {
                                    let size = 18.0 * scale;
                                    ui.horizontal(|ui| {
                                        ui.add(
                                            egui::Image::new((icon.uri, icon.bytes))
                                                .fit_to_exact_size(egui::Vec2::splat(size))
                                        );
                                        ui.button(&item.label).clicked()
                                    })
                                    .inner
                                }
                                else {
                                    ui.button(&item.label).clicked()
                                };
                                if clicked {
                                    if let Some(callback) = item.callback {
                                        let _ = panic::catch_unwind(AssertUnwindSafe(|| {
                                            callback(item.userdata as *mut c_void);
                                        }))
                                        .inspect_err(|_| {
                                            error!("plugin menu item callback panicked: {}", item.label);
                                        });
                                    }
                                }
                            }
                            ui.separator();
                        }

                        let plugin_sections = get_plugin_menu_sections();
                        if !plugin_sections.is_empty() {
                            for section in plugin_sections {
                                if let Some(title) = section.title.clone() {
                                    let icon = section.icon.clone();
                                    let size = 18.0 * scale;
                                    ui.horizontal(|ui| {
                                        if let Some(icon) = icon {
                                            ui.add(
                                                egui::Image::new((icon.uri, icon.bytes))
                                                    .fit_to_exact_size(egui::Vec2::splat(size))
                                            );
                                        }
                                        ui.heading(title);
                                    });
                                }
                                let _ = panic::catch_unwind(AssertUnwindSafe(|| {
                                    (section.callback)(ui as *mut _ as *mut c_void, section.userdata as *mut c_void);
                                }))
                                .inspect_err(|_| {
                                    error!("plugin menu section callback panicked");
                                });
                            }
                            ui.separator();
                        }

                        ui.heading(t!("menu.danger_zone_heading"));
                        ui.vertical(|ui| {
                            ui.label(t!("menu.danger_zone_warning"));
                        });
                        if ui.button(t!("menu.soft_restart")).clicked() {
                            show_window = Some(Box::new(SimpleYesNoDialog::new(&t!("confirm_dialog_title"), &t!("soft_restart_confirm_content"), |ok| {
                                if !ok { return; }
                                Thread::main_thread().schedule(|| {
                                    GameSystem::SoftwareReset(GameSystem::instance());
                                });
                            })));
                        }
                        #[cfg(not(target_os = "windows"))]
                        if ui.button(t!("menu.open_in_game_browser")).clicked() {
                            show_window = Some(Box::new(SimpleYesNoDialog::new(&t!("confirm_dialog_title"), &t!("in_game_browser_confirm_content"), |ok| {
                                if !ok { return; }
                                Thread::main_thread().schedule(|| {
                                    WebViewManager::quick_open(&t!("browser_dialog_title"), &Hachimi::instance().config.load().open_browser_url);
                                });
                            })));
                        }
                        if ui.button(t!("menu.toggle_game_ui")).clicked() {
                            Thread::main_thread().schedule(Self::toggle_game_ui);
                        }

                        #[cfg(target_os = "android")]
                        {
                            let padding = ime_scroll_padding(ui.ctx());
                            if padding > 0.0 {
                                ui.add_space(padding);
                            }
                        }
                    });
                });
            });
        }

        for message in drain_plugin_notifications() {
            self.show_notification(&message);
        }

        if !self.show_menu {
            if let Some(time) = self.menu_anim_time {
                if time.elapsed().as_secs_f32() >= self.context.style().animation_time {
                    self.menu_visible = false;
                }
            }
            else {
                self.menu_anim_time = Some(Instant::now());
            }
        }

        if let Some(content) = show_notification {
            self.show_notification(content.as_ref());
        }

        if let Some(window) = show_window {
            self.show_window(window);
        }
    }

    pub fn toggle_game_ui() {
        use crate::il2cpp::hook::{
            UnityEngine_CoreModule::{Object, Behaviour, GameObject},
            UnityEngine_UIModule::Canvas,
            Plugins::AnimateToUnity::AnRoot
        };

        let canvas_array = Object::FindObjectsOfType(Canvas::type_object(), true);
        let an_root_array = Object::FindObjectsOfType(AnRoot::type_object(), true);
        let canvas_iter = unsafe { canvas_array.as_slice().iter() };
        let an_root_iter = unsafe { an_root_array.as_slice().iter() };

        if unsafe { DISABLED_GAME_UIS.is_empty() } {
            for canvas in canvas_iter {
                if Behaviour::get_enabled(*canvas) {
                    Behaviour::set_enabled(*canvas, false);
                    unsafe { DISABLED_GAME_UIS.insert(*canvas); }
                }
            }
            for an_root in an_root_iter {
                let top_object = AnRoot::get__topObject(*an_root);
                if GameObject::get_activeSelf(top_object) {
                    GameObject::SetActive(top_object, false);
                    unsafe { DISABLED_GAME_UIS.insert(top_object); }
                }
            }
        }
        else {
            for canvas in canvas_iter {
                if unsafe { DISABLED_GAME_UIS.contains(canvas) } {
                    Behaviour::set_enabled(*canvas, true);
                }
            }
            for an_root in an_root_iter {
                let top_object = AnRoot::get__topObject(*an_root);
                if unsafe { DISABLED_GAME_UIS.contains(&top_object) } {
                    GameObject::SetActive(top_object, true);
                }
            }
            unsafe { DISABLED_GAME_UIS.clear(); }
        }
    }

    #[cfg(target_os = "windows")]
    fn run_vsync_combo(ui: &mut egui::Ui, value: &mut i32) {
        Self::run_combo(ui, "vsync_combo", value, &[
            (-1, &t!("default")),
            (0, &t!("off")),
            (1, &t!("on")),
            (2, "1/2"),
            (3, "1/3"),
            (4, "1/4")
        ]);
    }

    fn run_combo<T: PartialEq + Copy>(
        ui: &mut egui::Ui,
        id_child: impl std::hash::Hash,
        value: &mut T,
        choices: &[(T, &str)]
    ) -> bool {
        let mut selected = "Unknown";
        for choice in choices.iter() {
            if *value == choice.0 {
                selected = choice.1;
            }
        }

        let mut changed = false;
        egui::ComboBox::new(ui.id().with(id_child), "")
        .wrap_mode(egui::TextWrapMode::Wrap)
        .selected_text(selected)
        .show_ui(ui, |ui| {
            for choice in choices.iter() {
                changed |= ui.selectable_value(value, choice.0, choice.1).changed();
            }
        });

        changed
    }

    fn run_combo_menu<T: PartialEq + Copy>(
        ui: &mut egui::Ui,
        id_salt: impl std::hash::Hash,
        value: &mut T,
        choices: &[(T, &str)],
        search_term: &mut String,
    ) -> bool {
        let mut changed = false;
        let scale = get_scale(ui.ctx());
        let fixed_width = 135.0 * scale;
        let row_height = 24.0 * scale;
        let padding = ui.spacing().button_padding;

        let button_id = ui.make_persistent_id(id_salt);
        let popup_id = button_id.with("popup");

        let selected_text = choices.iter()
            .find(|(v, _)| v == value)
            .map(|(_, s)| *s)
            .unwrap_or("Unknown");

        let (rect, _) = ui.allocate_exact_size(egui::vec2(fixed_width, row_height), egui::Sense::hover());
        let button_res = ui.interact(rect, button_id, egui::Sense::click());

        if ui.is_rect_visible(rect) {
            let is_open = egui::Popup::is_id_open(ui.ctx(), popup_id);
            let visuals = if is_open {
                &ui.visuals().widgets.open
            } else {
                ui.style().interact(&button_res)
            };

            ui.painter().rect(
                rect.expand(visuals.expansion),
                visuals.corner_radius,
                visuals.weak_bg_fill,
                visuals.bg_stroke,
                egui::epaint::StrokeKind::Inside
            );

            let icon_size = 12.0 * scale; 
            let icon_rect = egui::Rect::from_center_size(
                egui::pos2(rect.right() - padding.x - icon_size / 2.0, rect.center().y),
                egui::vec2(icon_size, icon_size)
            );
            Self::down_triangle_icon(ui.painter(), icon_rect, visuals);

            let galley = ui.painter().layout_no_wrap(
                selected_text.to_owned(),
                egui::TextStyle::Button.resolve(ui.style()),
                visuals.text_color()
            );

            let text_pos = egui::pos2(
                rect.left() + padding.x,
                rect.center().y - galley.size().y / 2.0
            );
            ui.painter().galley(text_pos, galley, visuals.text_color());
        }

        egui::Popup::menu(&button_res)
        .id(popup_id)
        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
        .show(|ui| {
            ui.set_width(fixed_width);
            ui.set_max_width(fixed_width);

            ui.horizontal(|ui| {
                let _res = ui.add_sized(
                    [ui.available_width() - 30.0 * scale, row_height],
                    egui::TextEdit::singleline(search_term).hint_text(t!("search_filter"))
                );
                #[cfg(target_os = "android")]
                handle_android_keyboard(&_res, search_term);

                if ui.button("X").clicked() {
                    search_term.clear();
                }
            });

            ui.separator();

            egui::ScrollArea::vertical()
            .max_height(250.0 * scale)
            .hscroll(false)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.style_mut().wrap_mode = Some(egui::TextWrapMode::Wrap);

                ui.with_layout(egui::Layout::top_down_justified(egui::Align::Min), |ui| {
                    for (choice_val, label) in choices {
                        if !search_term.is_empty() && !label.to_lowercase().contains(&search_term.to_lowercase()) {
                            continue;
                        }
    
                        let is_selected = value == choice_val;
                        if ui.add(egui::Button::selectable(is_selected, *label)).clicked() {
                            *value = *choice_val;
                            changed = true;
                            egui::Popup::close_id(ui.ctx(), popup_id);
                            search_term.clear();
                        }
                    }
                });
            });
        });

        changed
    }

    // egui's code originally (https://github.com/emilk/egui/blob/main/crates/egui/src/containers/combo_box.rs)
    fn down_triangle_icon(painter: &egui::Painter, rect: egui::Rect, visuals: &egui::style::WidgetVisuals) {
        let rect = egui::Rect::from_center_size(
            rect.center(),
            egui::vec2(rect.width() * 0.7, rect.height() * 0.45)
        );

        painter.add(egui::Shape::convex_polygon(
            vec![
                rect.left_top(),
                rect.right_top(),
                rect.center_bottom()
            ],
            visuals.fg_stroke.color,
            visuals.fg_stroke
        ));
    }

    fn run_update_progress(&mut self) {
        let ctx = &self.context;
        let scale = get_scale(ctx);

        let progress = Hachimi::instance().tl_updater.progress().unwrap_or_else(|| {
            // Assume that update is complete
            self.update_progress_visible = false;
            tl_repo::UpdateProgress::new(1, 1)
        });
        let ratio = progress.current as f32 / progress.total as f32;

        egui::Area::new("update_progress".into())
        .fixed_pos(egui::Pos2 {
            x: 4.0 * scale,
            y: 4.0 * scale
        })
        .show(ctx, |ui| {
            egui::Frame::NONE
            .fill(BACKGROUND_COLOR)
            .inner_margin(egui::Margin::same((4.0 * scale) as i8))
            .corner_radius(4.0 * scale)
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.label(t!("tl_updater.title"));
                    ui.add_space(26.0 * scale);
                    ui.label(format!("{:.2}%", ratio * 100.0));
                });
                ui.add(
                    egui::ProgressBar::new(ratio)
                    .desired_height(4.0 * scale)
                    .desired_width(140.0 * scale)
                );
                ui.label(
                    egui::RichText::new(t!("tl_updater.warning"))
                    .font(egui::FontId::proportional(10.0 * scale))
                );
            });
        });
    }

    fn run_notifications(&mut self) {
        let mut offset: f32 = -16.0;
        self.notifications.retain_mut(|n| n.run(&self.context, &mut offset));
    }

    fn run_windows(&mut self) {
        self.windows.retain_mut(|w| w.run(&self.context));
    }

    pub fn is_empty(&self) -> bool {
        !self.splash_visible && !self.menu_visible && !self.update_progress_visible &&
        self.notifications.is_empty() && self.windows.is_empty()
    }

    pub fn is_consuming_input(&self) -> bool {
        self.menu_visible || !self.windows.is_empty()
    }

    pub fn is_consuming_input_atomic() -> bool {
        IS_CONSUMING_INPUT.load(atomic::Ordering::Relaxed)
    }

    pub fn toggle_menu(&mut self) {
        self.show_menu = !self.show_menu;
        // Menu is always visible on show, but not immediately invisible on hide
        if self.show_menu {
            self.menu_visible = true;
        }
        else {
            self.menu_anim_time = None;
        }
    }

    pub fn show_notification(&mut self, content: &str) {
        self.notifications.push(Notification::new(content.to_owned()));
    }

    pub fn show_window(&mut self, window: BoxedWindow) {
        self.windows.push(window);
    }
}

struct TweenInOutWithDelay {
    tween_time: f32,
    delay_duration: f32,
    easing: Easing,

    started: bool,
    delay_start: Option<Instant>
}

enum Easing {
    //Linear,
    //InQuad,
    OutQuad
}

impl TweenInOutWithDelay {
    fn new(tween_time: f32, delay_duration: f32, easing: Easing) -> TweenInOutWithDelay {
        TweenInOutWithDelay {
            tween_time,
            delay_duration,
            easing,

            started: false,
            delay_start: None
        }
    }

    fn run(&mut self, ctx: &egui::Context, id: egui::Id) -> Option<f32> {
        let anim_dir = if let Some(start) = self.delay_start {
            // Hold animation at peak position until duration passes
            start.elapsed().as_secs_f32() < self.delay_duration
        }
        else {
            // On animation start, initialize to 0.0. Next calls will start tweening to 1.0
            let v = self.started;
            self.started = true;
            v
        };
        let tween_val = ctx.animate_bool_with_time(id, anim_dir, self.tween_time);

        // Switch on delay when animation hits peak (next call makes tween_val < 1.0)
        if tween_val == 1.0 && self.delay_start.is_none() {
            self.delay_start = Some(Instant::now());
        }
        // Check if everything's done
        else if tween_val == 0.0 && self.delay_start.is_some() {
            return None;
        }

        Some(
            match self.easing {
                //Easing::Linear => tween_val,
                //Easing::InQuad => tween_val * tween_val,
                Easing::OutQuad => 1.0 - (1.0 - tween_val) * (1.0 - tween_val)
            }
        )
    }
}

// quick n dirty random id generator
fn random_id() -> egui::Id {
    egui::Id::new(egui::epaint::ahash::RandomState::new().hash_one(0))
}

struct Notification {
    content: String,
    tween: TweenInOutWithDelay,
    id: egui::Id
}

impl Notification {
    fn new(content: String) -> Notification {
        Notification {
            content,
            tween: TweenInOutWithDelay::new(0.2, 3.0, Easing::OutQuad),
            id: random_id()
        }
    }

    const WIDTH: f32 = 150.0;
    fn run(&mut self, ctx: &egui::Context, offset: &mut f32) -> bool {
        let scale = get_scale(ctx);

        let Some(tween_val) = self.tween.run(ctx, self.id.with("tween")) else {
            return false;
        };

        let frame_rect = egui::Area::new(self.id)
        .anchor(
            egui::Align2::RIGHT_BOTTOM,
            egui::Vec2::new(
                (Self::WIDTH * scale) * (1.0 - tween_val),
                *offset
            )
        )
        .show(ctx, |ui| {
            egui::Frame::NONE
            .fill(BACKGROUND_COLOR)
            .inner_margin(egui::Margin::symmetric(10, 8))
            .show(ui, |ui| {
                ui.set_width(Self::WIDTH * scale);
                ui.label(&self.content);
            }).response.rect
        }).inner;

        *offset -= (2.0 * scale) + frame_rect.height() * tween_val;
        true
    }
}

pub trait Window {
    fn run(&mut self, ctx: &egui::Context) -> bool;
}

// Shared window creation function
fn new_window<'a>(ctx: &egui::Context, id: egui::Id, title: impl Into<egui::WidgetText>) -> egui::Window<'a> {
    let scale = get_scale(ctx);
    let salt = get_scale_salt(ctx);

    egui::Window::new(title)
    .id(id.with(salt.to_bits()))
    .pivot(egui::Align2::CENTER_CENTER)
    .fixed_pos(ctx.viewport_rect().max / 2.0)
    .min_width(96.0 * scale)
    .max_width(320.0 * scale)
    .max_height(250.0 * scale)
    .collapsible(false)
    .resizable(false)
}

fn simple_window_layout(ui: &mut egui::Ui, id: egui::Id, add_contents: impl FnOnce(&mut egui::Ui), add_buttons: impl FnOnce(&mut egui::Ui)) {
    let builder = egui::UiBuilder::new()
        .id(id)
        .layout(egui::Layout::top_down(egui::Align::Center).with_cross_justify(true));

    ui.scope_builder(builder, |ui| {
        ui.with_layout(egui::Layout::top_down(egui::Align::Min), add_contents);

        ui.separator(); 

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), add_buttons);
    });
}

#[derive(Clone)]
struct StyleState {
    color: Color32,
    size: f32
    // had to remove sadly due to unnecessary bloat of font files
    // bold: bool,
    // italic: bool
}

fn parse_unity_text(ui: &egui::Ui, text: &str, wrap_width: f32) -> egui::text::LayoutJob {
    let mut job = egui::text::LayoutJob::default();
    job.wrap.max_width = wrap_width;
    // job.halign = egui::Align::Center;

    // default
    let mut state_stack = vec![StyleState {
        color: ui.style().visuals.text_color(),
        size: 12.0
        // bold: false,
        // italic: false
    }];

    let mut current_pos = 0;
    while current_pos < text.len() {
        if let Some(tag_start) = text[current_pos..].find("<") {
            let tag_start = current_pos + tag_start;

            if tag_start > current_pos {
                append_text(&mut job, &text[current_pos..tag_start], state_stack.last().unwrap());
            }

            if let Some(tag_end) = text[tag_start..].find(">") {
                let tag_end = tag_start + tag_end;
                let full_tag = &text[tag_start + 1..tag_end];
                
                if full_tag.starts_with("/") {
                    if state_stack.len() > 1 { state_stack.pop(); }
                } else {
                    let mut new_state = state_stack.last().unwrap().clone();
                    let parts: Vec<&str> = full_tag.split("=").collect();
                    let tag_name = parts[0].trim();

                    match tag_name {
                        // "b" => new_state.bold = true,
                        // "i" => new_state.italic = true,
                        "size" => if parts.len() > 1 {
                            if let Ok(s) = parts[1].trim_matches('"').parse::<f32>() {
                                new_state.size = s;
                            }
                        },
                        "color" => if parts.len() > 1 {
                            let color_val = parts[1].trim_matches('"');
                            new_state.color = parse_color(color_val).unwrap_or(new_state.color);
                        },
                        _ => {} // ignore <material> and <quad> 
                    }
                    state_stack.push(new_state);
                }
                current_pos = tag_end + 1;
            } else {
                append_text(&mut job, &text[tag_start..tag_start + 1], state_stack.last().unwrap());
                current_pos = tag_start + 1;
            }
        } else {
            append_text(&mut job, &text[current_pos..], state_stack.last().unwrap());
            break;
        }
    }
    job
}

fn append_text(job: &mut egui::text::LayoutJob, text: &str, state: &StyleState) {
    /* let family = if state.bold && state.italic {
        egui::FontFamily::Name("BoldItalicStack".into())
    } else if state.bold {
        egui::FontFamily::Name("BoldStack".into())
    } else if state.italic {
        egui::FontFamily::Name("ItalicStack".into())
    } else {
        egui::FontFamily::Proportional
    };*/
    let family = egui::FontFamily::Proportional;

    job.append(
        text,
        0.0,
        egui::text::TextFormat {
            font_id: egui::FontId::new(state.size, family),
            color: state.color,
            ..Default::default()
        }
    );
}

fn parse_color(val: &str) -> Option<Color32> {
    if val.starts_with('#') {
        let hex = val.trim_start_matches('#');
        match hex.len() {
            6 => { // RRGGBB
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                Some(Color32::from_rgb(r, g, b))
            }
            8 => { // RRGGBBAA
                let r = u8::from_str_radix(&hex[0..2], 16).ok()?;
                let g = u8::from_str_radix(&hex[2..4], 16).ok()?;
                let b = u8::from_str_radix(&hex[4..6], 16).ok()?;
                let a = u8::from_str_radix(&hex[6..8], 16).ok()?;
                Some(Color32::from_rgba_premultiplied(r, g, b, a))
            }
            _ => None
        }
    } else {
        get_unity_colors().get(val).cloned()
    }
}

fn rich_text_label(ui: &mut egui::Ui, text: &str) {
    let job = parse_unity_text(ui, text, ui.available_width());
    ui.add(egui::Label::new(job));
}

fn centered_and_wrapped_text(ui: &mut egui::Ui, text: &str) {
    let rect = ui.available_rect_before_wrap();

    let text_style = egui::TextStyle::Body;
    let text_font = ui.style().text_styles.get(&text_style).cloned().unwrap_or_default();
    let text_color = ui.style().visuals.text_color();

    let mut job = egui::text::LayoutJob::simple(
        text.to_owned(),
        text_font,
        text_color,
        rect.width()
    );
    job.halign = egui::Align::Center;

    let galley = ui.painter().layout_job(job);

    let text_rect = galley.rect;
    let text_size = text_rect.size();

    let center_pos = rect.min + (rect.size() - text_size) / 2.0;

    let paint_pos = center_pos - text_rect.min.to_vec2();
    ui.painter().galley(paint_pos, galley, text_color);
}

fn paginated_window_layout(
    ui: &mut egui::Ui,
    id: egui::Id,
    i: &mut usize,
    page_count: usize,
    allow_next: bool,
    add_page_content: impl FnOnce(&mut egui::Ui, usize)
) -> bool {
    let mut open = true;

    let builder = egui::UiBuilder::new()
        .id(id)
        .layout(egui::Layout::top_down(egui::Align::Center).with_cross_justify(true));

    ui.scope_builder(builder, |ui| {
        ui.with_layout(egui::Layout::top_down(egui::Align::Min), |ui| {
            add_page_content(ui, *i);
        });

        ui.separator();

        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
            if *i < page_count - 1 {
                if allow_next && ui.button(t!("next")).clicked() {
                    *i += 1;
                }
            } else {
                if ui.button(t!("done")).clicked() {
                    open = false;
                }
            }
            if *i > 0 && ui.button(t!("previous")).clicked() {
                *i -= 1;
            }
        });
    });

    open
}

fn async_request_ui_content<T: Send + Sync + 'static>(ui: &mut egui::Ui, request: Arc<AsyncRequest<T>>, add_contents: impl FnOnce(&mut egui::Ui, &T)) {
    let Some(result) = &**request.result.load() else {
        if !request.running() {
            request.call();
        }
        ui.centered_and_justified(|ui| {
            ui.label(t!("loading_label"));
        });
        return;
    };

    match result {
        Ok(v) => add_contents(ui, v),
        Err(e) => {
            let rect = ui.available_rect_before_wrap();

            let text_style = egui::TextStyle::Body;
            let text_font = ui.style().text_styles.get(&text_style).cloned().unwrap_or_default();
            let text_color = ui.visuals().text_color();

            let mut text_job = egui::text::LayoutJob::simple(e.to_string(), text_font, text_color, rect.width());
            text_job.halign = egui::Align::Center;
            let text_galley = ui.painter().layout_job(text_job.clone());
            let text_height = text_galley.size().y;

            let btn_text = t!("retry");
            let btn_style = egui::TextStyle::Button;
            let btn_font = ui.style().text_styles.get(&btn_style).cloned().unwrap_or_default();
            let btn_job = egui::text::LayoutJob::simple(btn_text.to_string(), btn_font, text_color, f32::INFINITY);
            let btn_galley = ui.painter().layout_job(btn_job);
            let btn_padding = ui.style().spacing.button_padding;
            let btn_height = btn_galley.size().y + btn_padding.y * 2.0;

            let spacing = ui.spacing().item_spacing.y;
            let total_height = text_height + spacing + btn_height;

            let center_y = rect.center().y;
            let top_y = (center_y - total_height / 2.0).max(rect.top());

            let content_rect = egui::Rect::from_min_size(
                egui::pos2(rect.left(), top_y),
                egui::vec2(rect.width(), total_height)
            );

            let builder = egui::UiBuilder::new().max_rect(content_rect);
            ui.scope_builder(builder, |ui| {
                ui.vertical_centered(|ui| {
                    ui.label(text_job);
                    if ui.button(btn_text).clicked() {
                        request.call();
                    }
                });
            });
        }
    }
}

pub struct SimpleYesNoDialog {
    title: String,
    content: String,
    callback: fn(bool),
    id: egui::Id
}

impl SimpleYesNoDialog {
    pub fn new(title: &str, content: &str, callback: fn(bool)) -> SimpleYesNoDialog {
        SimpleYesNoDialog {
            title: title.to_owned(),
            content: content.to_owned(),
            callback,
            id: random_id()
        }
    }
}

impl Window for SimpleYesNoDialog {
    fn run(&mut self, ctx: &egui::Context) -> bool {
        let mut open = true;
        let mut open2 = true;
        let mut result = false;

        new_window(ctx, self.id, &self.title)
        .open(&mut open)
        .show(ctx, |ui| {
            egui::TopBottomPanel::bottom(self.id.with("bottom_panel"))
            .show_inside(ui, |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    if ui.button(t!("no")).clicked() {
                        open2 = false;
                    }
                    if ui.button(t!("yes")).clicked() {
                        result = true;
                        open2 = false;
                    }
                })
            });

            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show_inside(ui, |ui| {
                centered_and_wrapped_text(ui, &self.content);
            });
        });

        if open && open2 {
            true
        }
        else {
            (self.callback)(result);
            false
        }
    }
}

pub struct SimpleOkDialog {
    title: String,
    content: String,
    callback: fn(),
    id: egui::Id
}

impl SimpleOkDialog {
    pub fn new(title: &str, content: &str, callback: fn()) -> SimpleOkDialog {
        SimpleOkDialog {
            title: title.to_owned(),
            content: content.to_owned(),
            callback,
            id: random_id()
        }
    }
}

impl Window for SimpleOkDialog {
    fn run(&mut self, ctx: &egui::Context) -> bool {
        let mut open = true;
        let mut open2 = true;

        new_window(ctx, self.id, &self.title)
        .open(&mut open)
        .show(ctx, |ui| {
            egui::TopBottomPanel::bottom(self.id.with("bottom_panel"))
            .show_inside(ui, |ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    if ui.button(t!("ok")).clicked() {
                        open2 = false;
                    }
                })
            });

            egui::CentralPanel::default()
                .frame(egui::Frame::NONE)
                .show_inside(ui, |ui| {
                centered_and_wrapped_text(ui, &self.content);
            });
        });

        if open && open2 {
            true
        }
        else {
            (self.callback)();
            false
        }
    }
}

pub struct SkillInfoDialog {
    skill_id: i32,
    name: String,
    desc: String,
    id: egui::Id
}

impl SkillInfoDialog {
    pub fn new(skill_id: &i32, title: &str, content: &str) -> SkillInfoDialog {
        SkillInfoDialog {
            skill_id: skill_id.to_owned(),
            name: title.to_owned(),
            desc: content.to_owned(),
            id: random_id()
        }
    }
}

impl Window for SkillInfoDialog {
    fn run(&mut self, ctx: &egui::Context) -> bool {
        let scale = get_scale(ctx);

        let mut open = true;
        let mut open2 = true;

        let light_frame = egui::Frame::window(&ctx.style())
            .fill(egui::Color32::from_gray(241))
            .corner_radius(10.0 * scale)
            .stroke(egui::Stroke::new(1.0, egui::Color32::from_gray(210)))
            .shadow(egui::Shadow::NONE);

        let window_res = new_window(ctx, self.id, "")
        .max_width(310.0 * scale)
        .max_height(240.0 * scale)
        .frame(light_frame)
        .title_bar(false)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.style_mut().visuals = egui::Visuals::light();

            ui.vertical(|ui| {
                egui::Frame::NONE
                .inner_margin(10.0 * scale)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()                    
                    .show(ui, |ui| {
                        ui.horizontal_wrapped(|ui| {
                            rich_text_label(ui, &format!("<b><size=16>{}</bold></b>", self.name));
                            rich_text_label(ui, &format!("#<b>{}</b>", self.skill_id.to_string()));
                        });

                        // ui.separator();                        
                        egui::Frame::NONE
                        .fill(egui::Color32::from_rgb(237, 228, 229))
                        .stroke(egui::Stroke::new(1.0, egui::Color32::from_gray(200)))
                        .corner_radius(4.0 * scale)
                        .inner_margin(5.0 * scale)
                        .show(ui, |ui| {
                            ui.set_min_width(ui.available_width());
                            rich_text_label(ui, &self.desc);
                        });
                    });
                });

                // ui.separator();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                    ui.add_space(6.0 * scale);
                    if ui.button(t!("ok")).clicked() {
                        open2 = false;
                    }
                });
                ui.add_space(5.0 * scale);
            });
        });

        if let Some(inner) = window_res {
            if ctx.input(|i| i.pointer.any_click()) {
                if let Some(pos) = ctx.input(|i| i.pointer.interact_pos()) {
                    if !inner.response.rect.contains(pos) {
                        open2 = false; 
                    }
                }
            }
        }

        open &= open2;
        open
    }
}

struct ConfigEditor {
    last_ptr_config: usize,
    config: hachimi::Config,
    id: egui::Id,
    current_tab: ConfigEditorTab
}

#[derive(Eq, PartialEq, Clone, Copy)]
enum ConfigEditorTab {
    General,
    Graphics,
    Gameplay
}

impl ConfigEditorTab {
    fn display_list() -> [(ConfigEditorTab, Cow<'static, str>); 3] {
        [
            (ConfigEditorTab::General, t!("config_editor.general_tab")),
            (ConfigEditorTab::Graphics, t!("config_editor.graphics_tab")),
            (ConfigEditorTab::Gameplay, t!("config_editor.gameplay_tab"))
        ]
    }
}

impl ConfigEditor {
    pub fn new() -> ConfigEditor {
        let handle = Hachimi::instance().config.load();
        ConfigEditor {
            last_ptr_config: Arc::as_ptr(&handle) as usize,
            config: (**Hachimi::instance().config.load()).clone(),
            id: random_id(),
            current_tab: ConfigEditorTab::General
        }
    }

    fn restore_defaults(&mut self) {
        let current_language = self.config.language;
        self.config = hachimi::Config::default();
        self.config.language = current_language;
    }

    fn option_slider<Num: egui::emath::Numeric>(ui: &mut egui::Ui, label: &str, value: &mut Option<Num>, range: RangeInclusive<Num>) {
        let mut checked = value.is_some();
        ui.label(label);
        ui.checkbox(&mut checked, t!("enable"));
        ui.end_row();

        if checked && value.is_none() {
            *value = Some(*range.start())
        }
        else if !checked && value.is_some() {
            *value = None;
        }

        if let Some(num) = value.as_mut() {
            ui.label("");
            let _res = ui.add(egui::Slider::new(num, range));
            #[cfg(target_os = "android")]
            handle_android_keyboard(&_res, num);
            ui.end_row();
        }
    }

    fn run_options_grid(config: &mut hachimi::Config, ui: &mut egui::Ui, tab: ConfigEditorTab) {
        let scale = get_scale(ui.ctx());

        match tab {
            ConfigEditorTab::General => {
                ui.label(t!("config_editor.language"));
                let lang_changed = Gui::run_combo(ui, "language", &mut config.language, Language::CHOICES);
                if lang_changed {
                    config.language.set_locale();
                }
                ui.end_row();

                ui.label(t!("config_editor.disable_overlay"));
                if ui.checkbox(&mut config.disable_gui, "").clicked() {
                    if config.disable_gui {
                        thread::spawn(|| {
                            Gui::instance().unwrap()
                            .lock().unwrap()
                            .show_window(Box::new(SimpleOkDialog::new(
                                &t!("warning"),
                                &t!("config_editor.disable_overlay_warning"),
                                || {}
                            )));
                        });
                    }
                }
                ui.end_row();

                ui.label(t!("config_editor.meta_index_url"));
                let _res = ui.add(egui::TextEdit::singleline(&mut config.meta_index_url).lock_focus(true));
                #[cfg(target_os = "android")]
                handle_android_keyboard(&_res, &mut config.meta_index_url);
                ui.end_row();

                ui.label(t!("config_editor.gui_scale"));
                let _res = ui.add(egui::Slider::new(&mut config.gui_scale, 0.25..=2.0).step_by(0.05));
                #[cfg(target_os = "android")]
                handle_android_keyboard(&_res, &mut config.gui_scale);
                ui.end_row();

                #[cfg(target_os = "windows")]
                {
                    ui.label(t!("config_editor.discord_rpc"));
                    ui.checkbox(&mut config.windows.discord_rpc, "");
                    ui.end_row();

                    ui.label(t!("config_editor.menu_open_key"));
                    ui.horizontal(|ui| {
                        ui.label(crate::windows::utils::vk_to_display_label(config.windows.menu_open_key));
                        if ui.button(t!("config_editor.menu_open_key_set")).clicked() {
                            crate::windows::wnd_hook::start_menu_key_capture();
                            thread::spawn(|| {
                                Gui::instance().unwrap()
                                .lock().unwrap()
                                .show_notification(&t!("notification.press_to_set_menu_key"));
                            });
                        }
                    });
                    ui.end_row();
                }

                ui.label(t!("config_editor.debug_mode"));
                ui.checkbox(&mut config.debug_mode, "");
                ui.end_row();

                ui.label(t!("config_editor.enable_file_logging"));
                ui.checkbox(&mut config.enable_file_logging, "");
                ui.end_row();

                ui.label(t!("config_editor.translator_mode"));
                ui.checkbox(&mut config.translator_mode, "");
                ui.end_row();

                ui.label(t!("config_editor.skip_first_time_setup"));
                ui.checkbox(&mut config.skip_first_time_setup, "");
                ui.end_row();

                ui.label(t!("config_editor.disable_auto_update_check"));
                ui.checkbox(&mut config.disable_auto_update_check, "");
                ui.end_row();

                ui.label(t!("config_editor.disable_translations"));
                ui.checkbox(&mut config.disable_translations, "");
                ui.end_row();

                ui.label(t!("config_editor.enable_ipc"));
                ui.checkbox(&mut config.enable_ipc, "");
                ui.end_row();

                ui.label(t!("config_editor.ipc_listen_all"));
                ui.checkbox(&mut config.ipc_listen_all, "");
                ui.end_row();

                ui.label(t!("config_editor.auto_translate_stories"));
                if ui.checkbox(&mut config.auto_translate_stories, "").clicked() {
                    if config.auto_translate_stories {
                        thread::spawn(|| {
                            Gui::instance().unwrap()
                            .lock().unwrap()
                            .show_window(Box::new(SimpleOkDialog::new(
                                &t!("warning"),
                                &t!("config_editor.auto_tl_warning"),
                                || {}
                            )));
                        });
                    }
                }
                ui.end_row();

                ui.label(t!("config_editor.auto_translate_ui"));
                if ui.checkbox(&mut config.auto_translate_localize, "").clicked() {
                    if config.auto_translate_localize {
                        thread::spawn(|| {
                            Gui::instance().unwrap()
                            .lock().unwrap()
                            .show_window(Box::new(SimpleOkDialog::new(
                                &t!("warning"),
                                &t!("config_editor.auto_tl_warning"),
                                || {}
                            )));
                        });
                    }
                }
                ui.end_row();
            },

            ConfigEditorTab::Graphics => {
                Self::option_slider(ui, &t!("config_editor.target_fps"), &mut config.target_fps, 30..=240);

                ui.label(t!("config_editor.virtual_resolution_multiplier"));
                let _res = ui.add(egui::Slider::new(&mut config.virtual_res_mult, 1.0..=4.0).step_by(0.1));
                #[cfg(target_os = "android")]
                handle_android_keyboard(&_res, &mut config.virtual_res_mult);
                ui.end_row();

                ui.label(t!("config_editor.ui_scale"));
                let _res = ui.add(egui::Slider::new(&mut config.ui_scale, 0.1..=10.0).step_by(0.05));
                #[cfg(target_os = "android")]
                handle_android_keyboard(&_res, &mut config.ui_scale);
                ui.end_row();

                ui.label(t!("config_editor.ui_animation_scale"));
                let _res = ui.add(egui::Slider::new(&mut config.ui_animation_scale, 0.1..=10.0).step_by(0.1));
                #[cfg(target_os = "android")]
                handle_android_keyboard(&_res, &mut config.ui_animation_scale);
                ui.end_row();

                ui.label(t!("config_editor.render_scale"));
                let _res = ui.add(egui::Slider::new(&mut config.render_scale, 0.1..=10.0).step_by(0.1));
                #[cfg(target_os = "android")]
                handle_android_keyboard(&_res, &mut config.render_scale);
                ui.end_row();

                ui.label(t!("config_editor.msaa"));
                Gui::run_combo(ui, "msaa", &mut config.msaa, &[
                    (MsaaQuality:: Disabled, &t!("default")),
                    (MsaaQuality::_2x, "2x"),
                    (MsaaQuality::_4x, "4x"),
                    (MsaaQuality::_8x, "8x")
                ]);
                ui.end_row();

                ui.label(t!("config_editor.aniso_level"));
                Gui::run_combo(ui, "aniso_level", &mut config.aniso_level, &[
                    (AnisoLevel::Default, &t!("default")),
                    (AnisoLevel::_2x, "2x"),
                    (AnisoLevel::_4x, "4x"),
                    (AnisoLevel::_8x, "8x"),
                    (AnisoLevel::_16x, "16x")
                ]);
                ui.end_row();

                ui.label(t!("config_editor.shadow_resolution"));
                Gui::run_combo(ui, "shadow_resolution", &mut config.shadow_resolution, &[
                    (ShadowResolution::Default, &t!("default")),
                    (ShadowResolution::_256, "256x"),
                    (ShadowResolution::_512, "512x"),
                    (ShadowResolution::_1024, "1K"),
                    (ShadowResolution::_2048, "2K"),
                    (ShadowResolution::_4096, "4K")
                ]);
                ui.end_row();

                ui.label(t!("config_editor.graphics_quality"));
                Gui::run_combo(ui, "graphics_quality", &mut config.graphics_quality, &[
                    (GraphicsQuality::Default, &t!("default")),
                    (GraphicsQuality::Toon1280, "Toon1280"),
                    (GraphicsQuality::Toon1280x2, "Toon1280x2"),
                    (GraphicsQuality::Toon1280x4, "Toon1280x4"),
                    (GraphicsQuality::ToonFull, "ToonFull"),
                    (GraphicsQuality::Max, "Max")
                ]);
                ui.end_row();

                #[cfg(target_os = "windows")]
                {
                    use crate::windows::hachimi_impl::{FullScreenMode, ResolutionScaling};

                    ui.label(t!("config_editor.vsync"));
                    Gui::run_vsync_combo(ui, &mut config.windows.vsync_count);
                    ui.end_row();

                    ui.label(t!("config_editor.auto_full_screen"));
                    ui.checkbox(&mut config.windows.auto_full_screen, "");
                    ui.end_row();

                    ui.label(t!("config_editor.full_screen_mode"));
                    Gui::run_combo(ui, "full_screen_mode", &mut config.windows.full_screen_mode, &[
                        (FullScreenMode::ExclusiveFullScreen, &t!("config_editor.full_screen_mode_exclusive")),
                        (FullScreenMode::FullScreenWindow, &t!("config_editor.full_screen_mode_borderless"))
                    ]);
                    ui.end_row();

                    ui.label(t!("config_editor.block_minimize_in_full_screen"));
                    ui.checkbox(&mut config.windows.block_minimize_in_full_screen, "");
                    ui.end_row();

                    ui.label(t!("config_editor.resolution_scaling"));
                    Gui::run_combo(ui, "resolution_scaling", &mut config.windows.resolution_scaling, &[
                        (ResolutionScaling::Default, &t!("config_editor.resolution_scaling_default")),
                        (ResolutionScaling::ScaleToScreenSize, &t!("config_editor.resolution_scaling_ssize")),
                        (ResolutionScaling::ScaleToWindowSize, &t!("config_editor.resolution_scaling_wsize"))
                    ]);
                    ui.end_row();

                    ui.label(t!("config_editor.window_always_on_top"));
                    ui.checkbox(&mut config.windows.window_always_on_top, "");
                    ui.end_row();
                }
            },

            ConfigEditorTab::Gameplay => {
                ui.label(t!("config_editor.physics_update_mode"));
                Gui::run_combo(ui, "physics_update_mode", &mut config.physics_update_mode, &[
                    (None, &t!("default")),
                    (SpringUpdateMode::ModeNormal.into(), "ModeNormal"),
                    (SpringUpdateMode::Mode60FPS.into(), "Mode60FPS"),
                    (SpringUpdateMode::SkipFrame.into(), "SkipFrame"),
                    (SpringUpdateMode::SkipFramePostAlways.into(), "SkipFramePostAlways")
                ]);
                ui.end_row();

                ui.label(t!("config_editor.story_choice_auto_select_delay"));
                let _res = ui.add(egui::Slider::new(&mut config.story_choice_auto_select_delay, 0.1..=10.0).step_by(0.05));
                #[cfg(target_os = "android")]
                handle_android_keyboard(&_res, &mut config.story_choice_auto_select_delay);
                ui.end_row();

                ui.label(t!("config_editor.story_text_speed_multiplier"));
                let _res = ui.add(egui::Slider::new(&mut config.story_tcps_multiplier, 0.1..=10.0).step_by(0.1));
                #[cfg(target_os = "android")]
                handle_android_keyboard(&_res, &mut config.story_tcps_multiplier);
                ui.end_row();

                ui.label(t!("config_editor.force_allow_dynamic_camera"));
                ui.checkbox(&mut config.force_allow_dynamic_camera, "");
                ui.end_row();

                ui.label(t!("config_editor.live_theater_allow_same_chara"));
                ui.checkbox(&mut config.live_theater_allow_same_chara, "");
                ui.end_row();

                ui.label(t!("config_editor.live_vocals_swap"));
                ui.horizontal(|ui| {
                    if ui.button(t!("open")).clicked() {
                        thread::spawn(|| {
                            Gui::instance().unwrap()
                            .lock().unwrap()
                            .show_window(Box::new(LiveVocalsSwapWindow::new()));
                        });
                    }
                });
                ui.end_row();

                ui.label(t!("config_editor.skill_info_dialog"));
                ui.checkbox(&mut config.skill_info_dialog, "");
                ui.end_row();

                ui.label(t!("config_editor.homescreen_bgseason"));
                Gui::run_combo(ui, "homescreen_bgseason", &mut config.homescreen_bgseason, &[
                    (BgSeason::None, &t!("default")),
                    // Season IDs from TextId enum
                    (BgSeason::Spring, unsafe { (*Localize::Get(1067)).as_utf16str() }.to_string().as_str()),
                    (BgSeason::Summer, unsafe { (*Localize::Get(1068)).as_utf16str() }.to_string().as_str()),
                    (BgSeason::Fall, unsafe { (*Localize::Get(1069)).as_utf16str() }.to_string().as_str()),
                    (BgSeason::Winter, unsafe { (*Localize::Get(1070)).as_utf16str() }.to_string().as_str()),
                    (BgSeason::CherryBlossom, unsafe { (*Localize::Get(1071)).as_utf16str() }.to_string().as_str())
                ]);
                ui.end_row();

                ui.label(t!("config_editor.disable_skill_name_translation"));
                ui.checkbox(&mut config.disable_skill_name_translation, "");
                ui.end_row();

                ui.label(t!("config_editor.hide_ingame_ui_hotkey"));
                if ui.checkbox(&mut config.hide_ingame_ui_hotkey, "").clicked() {
                    if config.hide_ingame_ui_hotkey {
                        thread::spawn(|| {
                            Gui::instance().unwrap()
                            .lock().unwrap()
                            .show_window(Box::new(SimpleOkDialog::new(
                                &t!("info"),
                                &t!("config_editor.hide_ingame_ui_hotkey_info"),
                                || {}
                            )));
                        });
                    }
                }
                ui.end_row();
            }
        }

        // Column widths workaround
        ui.horizontal(|ui| ui.add_space(100.0 * scale));
        ui.horizontal(|ui| ui.add_space(150.0 * scale));
        ui.end_row();
    }
}

impl Window for ConfigEditor {
    fn run(&mut self, ctx: &egui::Context) -> bool {
        let scale = get_scale(ctx);

        let mut open = true;
        let mut open2 = true;
        let global_handle = Hachimi::instance().config.load();
        let global_ptr = Arc::as_ptr(&global_handle) as usize;

        // sync config between diff windows
        if global_ptr != self.last_ptr_config {
            self.config = (**global_handle).clone();
            self.last_ptr_config = global_ptr;
        }
        let mut config = self.config.clone();
        #[cfg(target_os = "windows")]
        {
            config.windows.menu_open_key = global_handle.windows.menu_open_key;
        }
        let mut reset_clicked = false;

        new_window(ctx, self.id, t!("config_editor.title"))
        .open(&mut open)
        .show(ctx, |ui| {
            simple_window_layout(ui, self.id,
                |ui| {
                    egui::ScrollArea::horizontal()
                    .id_salt("tabs_scroll")
                    .show(ui, |ui| {
                        ui.horizontal(|ui| {
                            let style = ui.style_mut();
                            style.spacing.button_padding = egui::vec2(8.0, 5.0);
                            style.spacing.item_spacing = egui::Vec2::ZERO;
                            let widgets = &mut style.visuals.widgets;
                            widgets.inactive.corner_radius = egui::CornerRadius::ZERO;
                            widgets.hovered.corner_radius = egui::CornerRadius::ZERO;
                            widgets.active.corner_radius = egui::CornerRadius::ZERO;

                            for (tab, label) in ConfigEditorTab::display_list() {
                                if ui.selectable_label(self.current_tab == tab, label.as_ref()).clicked() {
                                    self.current_tab = tab;
                                }
                            }
                        });
                    });

                    ui.add_space(4.0);

                    egui::ScrollArea::vertical()
                    .id_salt("body_scroll")
                    .show(ui, |ui| {
                        egui::Frame::NONE
                        .inner_margin(egui::Margin::symmetric(8, 0))
                        .show(ui, |ui| {
                            egui::Grid::new(self.id.with("options_grid"))
                            .striped(true)
                            .num_columns(2)
                            .spacing([40.0 * scale, 4.0 * scale])
                            .show(ui, |ui| {
                                Self::run_options_grid(&mut config, ui, self.current_tab);
                            });
                        });
                        #[cfg(target_os = "android")]
                        {
                            let padding = ime_scroll_padding(ui.ctx());
                            if padding > 0.0 {
                                ui.add_space(padding);
                            }
                        }
                    });
                },
                |ui| {
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                        if ui.button(t!("config_editor.restore_defaults")).clicked() {
                            reset_clicked = true;
                        }

                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                            if ui.button(t!("cancel")).clicked() {
                                open2 = false;
                            }
                            if ui.button(t!("save")).clicked() {
                                save_and_reload_config(self.config.clone());
                                open2 = false;
                            }
                        });
                    });
                }
            );
        });

        self.config = config;

        if reset_clicked {
            self.restore_defaults();
        }

        open &= open2;
        if !open {
            let config_locale = Hachimi::instance().config.load().language.locale_str();
            if config_locale != &*rust_i18n::locale() {
                rust_i18n::set_locale(config_locale);
            }
        }

        open
    }
}

fn save_and_reload_config(config: hachimi::Config) {
    let notif = match Hachimi::instance().save_and_reload_config(config) {
        Ok(_) => t!("notification.config_saved").into_owned(),
        Err(e) => e.to_string()
    };

    // workaround since we can't get a mutable ref to the Gui and
    // locking the mutex on the current thread would cause a deadlock
    thread::spawn(move || {
        Gui::instance().unwrap()
        .lock().unwrap()
        .show_notification(&notif);
    });
}

struct FirstTimeSetupWindow {
    id: egui::Id,
    meta_index_url: String,
    config: hachimi::Config,
    index_request: Arc<AsyncRequest<Vec<RepoInfo>>>,
    current_page: usize,
    current_tl_repo: Option<String>,
    has_auto_selected: bool
}

impl FirstTimeSetupWindow {
    fn new() -> FirstTimeSetupWindow {
        let config = (**Hachimi::instance().config.load()).clone();
        FirstTimeSetupWindow {
            id: random_id(),
            meta_index_url: config.meta_index_url.clone(),
            config,
            index_request: Arc::new(tl_repo::new_meta_index_request()),
            current_page: 0,
            current_tl_repo: None,
            has_auto_selected: false
        }
    }
}

impl Window for FirstTimeSetupWindow {
    fn run(&mut self, ctx: &egui::Context) -> bool {
        let mut open = true;
        let mut page_open = true;

        new_window(ctx, self.id, t!("first_time_setup.title"))
        .open(&mut open)
        .show(ctx, |ui| {
            let allow_next = match self.current_page {
                1 => {
                    (**self.index_request.result.load()).as_ref().map_or(false, |r| r.is_ok())
                },
                _ => true
            };

            page_open = paginated_window_layout(ui, self.id, &mut self.current_page, 3, allow_next, |ui, i| {
                match i {
                    0 => {
                        ui.heading(t!("first_time_setup.welcome_heading"));
                        ui.separator();
                        ui.horizontal(|ui| {
                            ui.label(t!("config_editor.language"));
                            let mut language = self.config.language;
                            let lang_changed = Gui::run_combo(ui, "language", &mut language, Language::CHOICES);
                            if lang_changed {
                                self.config.language = language;
                                save_and_reload_config(self.config.clone());
                                self.current_tl_repo = None;
                            }
                        });
                        ui.horizontal(|ui| {
                            ui.label(t!("config_editor.meta_index_url"));
                            let res = ui.add(egui::TextEdit::singleline(&mut self.meta_index_url).lock_focus(true));
                            #[cfg(target_os = "android")]
                            handle_android_keyboard(&res, &mut self.meta_index_url);
                            #[cfg(target_os = "windows")]
                            if res.has_focus() {
                                ui.memory_mut(|mem| mem.set_focus_lock_filter(
                                    res.id,
                                    egui::EventFilter {
                                        tab: true,
                                        horizontal_arrows: true,
                                        vertical_arrows: true,
                                        escape: false,
                                        ..Default::default()
                                    }
                                ));
                            }

                            if res.lost_focus() {
                                if self.meta_index_url != self.config.meta_index_url {
                                    self.config.meta_index_url = self.meta_index_url.clone();
                                    save_and_reload_config(self.config.clone());
                                    self.index_request = Arc::new(tl_repo::new_meta_index_request());
                                }
                            }
                        });
                        ui.separator();
                        ui.label(t!("first_time_setup.welcome_content"));
                    }
                    1 => {
                        ui.heading(t!("first_time_setup.translation_repo_heading"));
                        ui.separator();
                        ui.label(t!("first_time_setup.select_translation_repo"));
                        ui.add_space(4.0);

                        async_request_ui_content(ui, self.index_request.clone(), |ui, repo_list| {
                            let hachimi = Hachimi::instance();
                            let current_lang_str = self.config.language.locale_str();

                            let mut filtered_repos: Vec<_> = repo_list.iter()
                                .filter(|repo| repo.region == hachimi.game.region)
                                .collect();

                            if !self.has_auto_selected && self.current_tl_repo.is_none() {
                                if let Some(matched) = filtered_repos.iter().find(|r| r.is_recommended(current_lang_str)) {
                                    self.current_tl_repo = Some(matched.index.clone());
                                }
                                self.has_auto_selected = true;
                            }
  
                            filtered_repos.sort_by_key(|repo| !repo.is_recommended(current_lang_str));
                            
                            egui::ScrollArea::vertical().show(ui, |ui| {
                                egui::Frame::NONE
                                .inner_margin(egui::Margin::symmetric(8, 0))
                                .show(ui, |ui| {
                                    if filtered_repos.is_empty() {
                                        ui.label(t!("first_time_setup.no_compatible_repo"));
                                        return;
                                    }
                                    ui.radio_value(&mut self.current_tl_repo, None, t!("first_time_setup.skip_translation"));

                                    let mut last_section: Option<bool> = None;

                                    for repo in filtered_repos.iter() {
                                        let is_matched = repo.is_recommended(current_lang_str);
                                        let is_selected = self.current_tl_repo.as_ref() == Some(&repo.index);
                                        
                                        // Add separator before switching from matched to unmatched
                                        if let Some(prev_matched) = last_section {
                                            if prev_matched != is_matched {
                                                ui.separator();
                                            }
                                        }

                                        // Visual indicator for auto-selected matched language repo
                                        if is_matched && is_selected {
                                            let repo_label = format!("★ {}", repo.name);
                                            ui.radio_value(&mut self.current_tl_repo, Some(repo.index.clone()), repo_label);
                                            if let Some(short_desc) = &repo.short_desc {
                                                ui.label(egui::RichText::new(short_desc).small());
                                            }
                                        } else {
                                            ui.radio_value(&mut self.current_tl_repo, Some(repo.index.clone()), &repo.name);
                                            if let Some(short_desc) = &repo.short_desc {
                                                ui.label(egui::RichText::new(short_desc).small());
                                            }
                                        }
                                        
                                        last_section = Some(is_matched);
                                    }
                                });
                                #[cfg(target_os = "android")]
                                {
                                    let padding = ime_scroll_padding(ui.ctx());
                                    if padding > 0.0 {
                                        ui.add_space(padding);
                                    }
                                }
                            });
                        });
                    }
                    2 => {
                        ui.heading(t!("first_time_setup.complete_heading"));
                        ui.separator();
                        ui.label(t!("first_time_setup.complete_content"));
                    }
                    _ => {}
                }
            });
        });

        let open_res = open && page_open;
        if !open_res {
            self.config.skip_first_time_setup = true;

            if !page_open {
                self.config.translation_repo_index = self.current_tl_repo.clone();
            }

            save_and_reload_config(self.config.clone());

            if !page_open {
                Hachimi::instance().tl_updater.clone().check_for_updates(false);
            }
        }

        open_res
    }
}

struct LiveVocalsSwapWindow {
    id: egui::Id,
    config: hachimi::Config,
    chara_choices: Vec<(i32, String)>,
    search_term: String
}

impl LiveVocalsSwapWindow {
    fn new() -> LiveVocalsSwapWindow {
        let hachimi = Hachimi::instance();
        let chara_data = hachimi.chara_data.load_full();
        let mut chara_choices: Vec<(i32, String)> = Vec::new();
        chara_choices.push((0, t!("default").into_owned()));

        if let Some(data) = chara_data.as_ref() {
            for &id in &data.chara_ids {
                chara_choices.push((id, data.get_name(id)));
            }
        }
        chara_choices.sort_by_key(|choice| choice.0);

        LiveVocalsSwapWindow {
            id: random_id(),
            config: (**hachimi.config.load()).clone(),
            chara_choices,
            search_term: String::new()
        }
    }
}

impl Window for LiveVocalsSwapWindow {
    fn run(&mut self, ctx: &egui::Context) -> bool {
        let scale = get_scale(ctx);
        let mut open = true;
        let mut open2 = true;
        let mut save_clicked = false;

        let combo_items: Vec<(i32, &str)> = self.chara_choices
            .iter()
            .map(|&(id, ref name)| (id, name.as_str()))
            .collect();

        new_window(ctx, self.id, t!("config_editor.live_vocals_swap"))
        .open(&mut open)
        .show(ctx, |ui| {
            simple_window_layout(ui, self.id,
                |ui| {
                    egui::Frame::NONE
                    .inner_margin(egui::Margin::symmetric(8, 0))
                    .show(ui, |ui| {
                        egui::Grid::new(self.id.with("live_vocals_swap_grid"))
                        .striped(true)
                        .num_columns(2)
                        .spacing([40.0 * scale, 4.0 * scale])
                        .show(ui, |ui| {
                            for i in 0..6 {
                                ui.label(t!("config_editor.live_vocals_swap_character_n", index = i + 1));
                                Gui::run_combo_menu(ui, egui::Id::new("vocals_swap").with(i), &mut self.config.live_vocals_swap[i], &combo_items, &mut self.search_term);
                                ui.end_row();
                            }
                        });
                    });
                },
                |ui| {
                    ui.with_layout(egui::Layout::left_to_right(egui::Align::Min), |ui| {
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Min), |ui| {
                            if ui.button(t!("cancel")).clicked() {
                                open2 = false;
                            }
                            if ui.button(t!("save")).clicked() {
                                save_clicked = true;
                                open2 = false;
                            }
                        });
                    });
                }
            );
        });

        if save_clicked {
            save_and_reload_config(self.config.clone());
        }

        open &= open2;
        open
    }
}

struct AboutWindow {
    id: egui::Id
}

impl AboutWindow {
    fn new() -> AboutWindow {
        AboutWindow {
            id: random_id()
        }
    }
}

impl Window for AboutWindow {
    fn run(&mut self, ctx: &egui::Context) -> bool {
        let scale = get_scale(ctx);
        let mut open = true;

        new_window(ctx, self.id, t!("about.title"))
        .max_width(310.0 * scale)
        .open(&mut open)
        .show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.add(Gui::icon_2x(ctx));
                ui.vertical(|ui| {
                    ui.heading(t!("hachimi"));
                    ui.label(env!("HACHIMI_DISPLAY_VERSION"));
                });
            });
            ui.label(t!("about.copyright", year = Utc::now().year()));
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing.x = 8.0;

                if ui.button(t!("about.view_license")).clicked() {
                    thread::spawn(|| {
                        Gui::instance().unwrap()
                        .lock().unwrap()
                        .show_window(Box::new(LicenseWindow::new()));
                    });
                }
                ui.end_row();

                if ui.button(t!("about.open_website")).clicked() {
                    Application::OpenURL(WEBSITE_URL.to_il2cpp_string());
                }

                if ui.button(t!("about.view_source_code")).clicked() {
                    Application::OpenURL(format!("https://github.com/{}", REPO_PATH).to_il2cpp_string());
                }
            });
        });

        open
    }
}

struct LicenseWindow {
    id: egui::Id
}

impl LicenseWindow {
    fn new() -> LicenseWindow {
        LicenseWindow {
            id: random_id()
        }
    }
}

impl Window for LicenseWindow {
    fn run(&mut self, ctx: &egui::Context) -> bool {
        let mut open = true;

        new_window(ctx, self.id, t!("license.title"))
        .open(&mut open)
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| {
                ui.heading(t!("hachimi"));
                ui.collapsing(t!("license.gpl_v3_only_notice"), |ui| {
                    ui.add(egui::TextEdit::multiline(&mut include_str!("../../LICENSE"))
                        .font(egui::TextStyle::Monospace)
                        .desired_rows(10)
                        .interactive(false)
                    );
                });
                ui.separator();

                ui.heading("Open Font Licenses (OFL)");
                ui.label(t!("license.ofl_fonts_header"));
                ui.group(|ui| {
                    ui.label(t!("license.font_font_awesome"));
                    ui.label(t!("license.font_noto_sans"));
                    ui.label(t!("license.font_noto_sans_jp"));
                });

                ui.add_space(4.0);
                ui.collapsing(t!("license.ofl_notice"), |ui| {
                    ui.add(egui::TextEdit::multiline(&mut include_str!("../../assets/fonts/OFL.txt"))
                        .font(egui::TextStyle::Monospace)
                        .desired_rows(10)
                        .interactive(false)
                    );
                });

                ui.add_space(10.0);
                ui.separator();

                ui.heading(t!("license.font_alibaba_header"));
                ui.label(t!("license.font_alibaba_body"));
            });
        });

        open
    }
}

pub struct PersistentMessageWindow {
    id: egui::Id,
    title: String,
    content: String,
    show: Arc<AtomicBool>
}

impl PersistentMessageWindow {
    pub fn new(title: &str, content: &str, show: Arc<AtomicBool>) -> PersistentMessageWindow {
        PersistentMessageWindow {
            id: random_id(),
            title: title.to_owned(),
            content: content.to_owned(),
            show
        }
    }
}

impl Window for PersistentMessageWindow {
    fn run(&mut self, ctx: &egui::Context) -> bool {
        new_window(ctx, self.id, &self.title)
        .show(ctx, |ui| {
            simple_window_layout(ui, self.id,
                |ui| {
                    ui.centered_and_justified(|ui| {
                        ui.label(&self.content);
                    });
                },
                |_| {
                }
            );
        });

        self.show.load(atomic::Ordering::Relaxed)
    }
}
