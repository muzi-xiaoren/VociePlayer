//! egui 界面 + 状态管理，把配置、音频引擎、热键、profile 串起来。

use crate::audio::{self, AudioEngine};
use crate::config::AppConfig;
use crate::hotkeys::{self, HkAction, Hotkeys};
use crate::platform;
use crate::profile::{self, Profile, Sound};
use std::time::{Duration, Instant};

/// 正在为「谁」捕获快捷键。
#[derive(Clone, Copy, PartialEq)]
enum CaptureTarget {
    Sound(usize),
    Stop,
}

/// UI 一帧里收集下来、帧末统一处理的动作（避免边遍历边改状态的借用冲突）。
#[derive(Default)]
struct Pending {
    rebuild_engine: bool,
    switch_profile: Option<String>,
    new_profile: Option<String>,
    play: Vec<usize>,
    capture: Option<CaptureTarget>,
    clear_hotkey: Vec<usize>,
    clear_stop: bool,
    set_volume: Vec<(usize, f32)>,
    open_folder: bool,
    reregister: bool,
}

pub struct App {
    config: AppConfig,
    engine: Option<AudioEngine>,
    engine_error: Option<String>,
    hotkeys: Option<Hotkeys>,
    profiles: Vec<String>,
    profile: Option<Profile>,
    out_devices: Vec<String>,
    in_devices: Vec<String>,
    vbcable: bool,
    capturing: Option<CaptureTarget>,
    new_profile_name: String,
    last_scan: Instant,
    last_signature: Vec<String>,
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        install_cjk_fonts(&cc.egui_ctx);

        let mut config = AppConfig::load();
        let out_devices = audio::output_devices();
        let in_devices = audio::input_devices();
        let vbcable = audio::vbcable_installed();

        // 首次运行、还没选输出设备时，自动选中 VB-CABLE。
        if config.output_device.is_none() {
            config.output_device = audio::guess_cable_output();
        }

        // 确定当前 profile。
        let profiles = profile::list_profiles();
        let active = config
            .active_profile
            .clone()
            .filter(|n| profiles.contains(n))
            .or_else(|| profiles.first().cloned());
        config.active_profile = active.clone();
        let profile = active.as_deref().map(Profile::load);
        let last_signature = profile
            .as_ref()
            .map(|p| Profile::file_signature(&p.dir))
            .unwrap_or_default();

        let hotkeys = match Hotkeys::new() {
            Ok(h) => Some(h),
            Err(e) => {
                log::error!("初始化全局热键失败：{e}");
                None
            }
        };

        let mut app = Self {
            config,
            engine: None,
            engine_error: None,
            hotkeys,
            profiles,
            profile,
            out_devices,
            in_devices,
            vbcable,
            capturing: None,
            new_profile_name: String::new(),
            last_scan: Instant::now(),
            last_signature,
        };
        app.config.save();
        app.rebuild_engine();
        app.reregister_hotkeys();
        app
    }

    fn rebuild_engine(&mut self) {
        match AudioEngine::new(&self.config) {
            Ok(e) => {
                self.engine = Some(e);
                self.engine_error = None;
            }
            Err(e) => {
                self.engine = None;
                self.engine_error = Some(format!("{e:#}"));
            }
        }
    }

    fn reregister_hotkeys(&mut self) {
        let Some(hk) = self.hotkeys.as_mut() else {
            return;
        };
        hk.clear();
        if let Some(p) = &self.profile {
            for s in &p.sounds {
                if let Some(combo) = &s.hotkey {
                    hk.register(
                        combo,
                        HkAction::Play {
                            path: s.path.clone(),
                            volume: s.volume,
                        },
                    );
                }
            }
        }
        if let Some(stop) = &self.config.stop_hotkey {
            hk.register(stop, HkAction::StopAll);
        }
    }

    fn switch_profile(&mut self, name: &str) {
        self.config.active_profile = Some(name.to_string());
        let p = Profile::load(name);
        self.last_signature = Profile::file_signature(&p.dir);
        self.profile = Some(p);
        self.config.save();
        self.reregister_hotkeys();
    }

    fn refresh_devices(&mut self) {
        self.out_devices = audio::output_devices();
        self.in_devices = audio::input_devices();
        self.vbcable = audio::vbcable_installed();
    }

    /// 每隔 ~1.5 秒轻量比对文件夹，检测到有音频增删就重载（实现「丢进去自动识别」）。
    fn maybe_rescan(&mut self) {
        if self.last_scan.elapsed() < Duration::from_millis(1500) {
            return;
        }
        self.last_scan = Instant::now();
        let info = self.profile.as_ref().map(|p| (p.name.clone(), p.dir.clone()));
        if let Some((name, dir)) = info {
            let sig = Profile::file_signature(&dir);
            if sig != self.last_signature {
                self.last_signature = sig;
                self.profile = Some(Profile::load(&name));
                self.reregister_hotkeys();
            }
        }
    }

    /// 处理正在进行的快捷键捕获：读取本帧按键事件。
    fn handle_capture(&mut self, ctx: &egui::Context) {
        if self.capturing.is_none() {
            return;
        }
        // 返回 Some(Some(combo))=捕获成功；Some(None)=Esc 取消；None=还没按。
        let result = ctx.input(|i| {
            for ev in &i.events {
                if let egui::Event::Key {
                    key,
                    pressed: true,
                    modifiers,
                    ..
                } = ev
                {
                    if *key == egui::Key::Escape {
                        return Some(None);
                    }
                    if let Some(combo) = hotkeys::from_egui(modifiers, *key) {
                        return Some(Some(combo));
                    }
                }
            }
            None
        });

        let Some(outcome) = result else { return };
        let target = self.capturing.take();
        if let (Some(combo), Some(target)) = (outcome, target) {
            match target {
                CaptureTarget::Sound(i) => {
                    if let Some(p) = self.profile.as_mut() {
                        if let Some(s) = p.sounds.get_mut(i) {
                            s.hotkey = Some(combo);
                        }
                        p.save_bindings();
                    }
                }
                CaptureTarget::Stop => {
                    self.config.stop_hotkey = Some(combo);
                    self.config.save();
                }
            }
            self.reregister_hotkeys();
        }
    }

    fn apply(&mut self, pending: Pending) {
        let mut need_reregister = pending.reregister;

        for (i, v) in pending.set_volume {
            if let Some(p) = self.profile.as_mut() {
                if let Some(s) = p.sounds.get_mut(i) {
                    s.volume = v;
                }
                p.save_bindings();
            }
            let _ = i;
            need_reregister = true;
        }

        for i in pending.clear_hotkey {
            if let Some(p) = self.profile.as_mut() {
                if let Some(s) = p.sounds.get_mut(i) {
                    s.hotkey = None;
                }
                p.save_bindings();
            }
            need_reregister = true;
        }

        if pending.clear_stop {
            self.config.stop_hotkey = None;
            self.config.save();
            need_reregister = true;
        }

        for i in pending.play {
            if let (Some(p), Some(e)) = (&self.profile, &self.engine) {
                if let Some(s) = p.sounds.get(i) {
                    e.play(&s.path, s.volume);
                }
            }
        }

        if pending.open_folder {
            if let Some(p) = &self.profile {
                platform::open_folder(&p.dir);
            }
        }

        if let Some(t) = pending.capture {
            self.capturing = Some(t);
        }

        if let Some(name) = pending.new_profile {
            let name = name.trim().to_string();
            if !name.is_empty() && profile::create_profile(&name).is_ok() {
                self.profiles = profile::list_profiles();
                self.switch_profile(&name);
                self.new_profile_name.clear();
            }
        }

        if let Some(name) = pending.switch_profile {
            self.switch_profile(&name);
        }

        if pending.rebuild_engine {
            self.rebuild_engine();
        }

        if need_reregister {
            self.reregister_hotkeys();
        }
    }

    fn ui_settings(
        &mut self,
        ui: &mut egui::Ui,
        out_devices: &[String],
        in_devices: &[String],
        pending: &mut Pending,
    ) {
        ui.heading("VociePlayer");
        ui.add_space(4.0);

        // VB-CABLE 状态
        if self.vbcable {
            ui.colored_label(egui::Color32::from_rgb(60, 170, 90), "✔ 已检测到 VB-CABLE");
        } else {
            ui.colored_label(
                egui::Color32::from_rgb(200, 120, 40),
                "⚠ 未检测到 VB-CABLE（游戏里听不到音效）",
            );
            ui.horizontal(|ui| {
                if ui.button("打开 VB-CABLE 下载页").clicked() {
                    platform::open_url(platform::VBCABLE_URL);
                }
                if ui.button("我装好了，重新检测").clicked() {
                    self.refresh_devices();
                    pending.rebuild_engine = true;
                }
            });
        }
        ui.separator();

        // —— 设备选择 ——
        device_combo(
            ui,
            "输出设备（选 CABLE Input）",
            &mut self.config.output_device,
            out_devices,
            "系统默认",
            &mut pending.rebuild_engine,
        );
        device_combo(
            ui,
            "麦克风",
            &mut self.config.input_device,
            in_devices,
            "系统默认",
            &mut pending.rebuild_engine,
        );
        device_combo(
            ui,
            "监听设备（可选，让自己也能听到）",
            &mut self.config.monitor_device,
            out_devices,
            "不监听",
            &mut pending.rebuild_engine,
        );
        if pending.rebuild_engine {
            self.config.save();
        }

        ui.separator();

        // —— 麦克风转发开关 ——
        if ui
            .checkbox(&mut self.config.mic_passthrough, "转发麦克风（边说话边放音效）")
            .changed()
        {
            if let Some(e) = &self.engine {
                e.set_mic_passthrough(self.config.mic_passthrough);
            }
            self.config.save();
        }

        // —— 音效音量 ——
        ui.horizontal(|ui| {
            ui.label("音效音量");
            let resp = ui.add(egui::Slider::new(&mut self.config.effect_volume, 0.0..=1.5));
            if resp.changed() {
                if let Some(e) = &mut self.engine {
                    e.set_effect_volume(self.config.effect_volume);
                }
                self.config.save();
            }
        });

        // —— 开机自启 ——
        if ui
            .checkbox(&mut self.config.autostart, "开机自启动")
            .changed()
        {
            if let Err(e) = platform::set_autostart(self.config.autostart) {
                log::error!("设置开机自启失败：{e}");
            }
            self.config.save();
        }

        // —— 引擎错误 ——
        if let Some(err) = &self.engine_error {
            ui.separator();
            ui.colored_label(egui::Color32::from_rgb(210, 70, 70), format!("音频引擎错误：{err}"));
            if ui.button("重试").clicked() {
                pending.rebuild_engine = true;
            }
        }
    }

    fn ui_sounds(
        &mut self,
        ui: &mut egui::Ui,
        profiles: &[String],
        sounds: &[Sound],
        pending: &mut Pending,
    ) {
        // —— profile 选择 / 新建 ——
        ui.horizontal(|ui| {
            let cur = self
                .config
                .active_profile
                .clone()
                .unwrap_or_else(|| "（无）".to_string());
            egui::ComboBox::from_label("配置")
                .selected_text(cur)
                .show_ui(ui, |ui| {
                    for name in profiles {
                        let selected = self.config.active_profile.as_deref() == Some(name);
                        if ui.selectable_label(selected, name).clicked() && !selected {
                            pending.switch_profile = Some(name.clone());
                        }
                    }
                });
            if ui.button("📂 打开文件夹").clicked() {
                pending.open_folder = true;
            }
        });
        ui.horizontal(|ui| {
            ui.label("新建配置：");
            ui.text_edit_singleline(&mut self.new_profile_name);
            if ui.button("新建").clicked() {
                pending.new_profile = Some(self.new_profile_name.clone());
            }
        });

        // —— 停止键 ——
        ui.horizontal(|ui| {
            let label = self
                .config
                .stop_hotkey
                .clone()
                .unwrap_or_else(|| "未设置".to_string());
            ui.label(format!("停止所有音效：{label}"));
            if ui.button("设置快捷键").clicked() {
                pending.capture = Some(CaptureTarget::Stop);
            }
            if self.config.stop_hotkey.is_some() && ui.button("清除").clicked() {
                pending.clear_stop = true;
            }
        });

        ui.separator();

        if sounds.is_empty() {
            ui.label("这个配置还没有音频。点「打开文件夹」把 mp3 / wav 拖进去，会自动出现在这里。");
            return;
        }

        // —— 音效列表 ——
        egui::ScrollArea::vertical().show(ui, |ui| {
            for (i, s) in sounds.iter().enumerate() {
                ui.horizontal(|ui| {
                    if ui.button("▶").clicked() {
                        pending.play.push(i);
                    }
                    ui.label(&s.name);

                    // 右侧对齐放快捷键 / 音量控制
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        // 单独音量
                        let mut v = s.volume;
                        let resp = ui.add(
                            egui::Slider::new(&mut v, 0.0..=1.5)
                                .show_value(false)
                                .fixed_decimals(1),
                        );
                        if resp.changed() {
                            pending.set_volume.push((i, v));
                        }

                        // 快捷键
                        let capturing_this = self.capturing == Some(CaptureTarget::Sound(i));
                        if s.hotkey.is_some() && ui.button("✖").clicked() {
                            pending.clear_hotkey.push(i);
                        }
                        let btn_label = if capturing_this {
                            "按下快捷键…（Esc 取消）".to_string()
                        } else {
                            s.hotkey.clone().unwrap_or_else(|| "设快捷键".to_string())
                        };
                        if ui.button(btn_label).clicked() && !capturing_this {
                            pending.capture = Some(CaptureTarget::Sound(i));
                        }
                    });
                });
            }
        });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // 让主循环持续跑，才能轮询热键、重扫文件夹。
        ctx.request_repaint_after(Duration::from_millis(150));

        // 1) 处理全局热键事件
        let actions = self.hotkeys.as_ref().map(|h| h.poll()).unwrap_or_default();
        for a in actions {
            match a {
                HkAction::Play { path, volume } => {
                    if let Some(e) = &self.engine {
                        e.play(&path, volume);
                    }
                }
                HkAction::StopAll => {
                    if let Some(e) = &self.engine {
                        e.stop_all();
                    }
                }
            }
        }

        // 2) 文件夹自动重扫
        self.maybe_rescan();

        // 3) 快捷键捕获
        self.handle_capture(ctx);

        // 4) 界面（设备/profile 列表先克隆成局部，避开借用冲突）
        let out_devices = self.out_devices.clone();
        let in_devices = self.in_devices.clone();
        let profiles = self.profiles.clone();
        let sounds = self.profile.as_ref().map(|p| p.sounds.clone()).unwrap_or_default();
        let mut pending = Pending::default();

        egui::TopBottomPanel::top("settings")
            .resizable(false)
            .show(ctx, |ui| {
                self.ui_settings(ui, &out_devices, &in_devices, &mut pending);
            });
        egui::CentralPanel::default().show(ctx, |ui| {
            self.ui_sounds(ui, &profiles, &sounds, &mut pending);
        });

        // 5) 帧末统一处理
        self.apply(pending);
    }
}

/// 一个「选择设备」下拉框。选择变化时把 `changed` 置 true。
fn device_combo(
    ui: &mut egui::Ui,
    label: &str,
    selected: &mut Option<String>,
    devices: &[String],
    none_text: &str,
    changed: &mut bool,
) {
    egui::ComboBox::from_label(label)
        .selected_text(selected.clone().unwrap_or_else(|| none_text.to_string()))
        .show_ui(ui, |ui| {
            if ui.selectable_label(selected.is_none(), none_text).clicked() && selected.is_some() {
                *selected = None;
                *changed = true;
            }
            for d in devices {
                let is_sel = selected.as_deref() == Some(d);
                if ui.selectable_label(is_sel, d).clicked() && !is_sel {
                    *selected = Some(d.clone());
                    *changed = true;
                }
            }
        });
}

/// 加载中文字体（否则中文界面全是方块）。优先 Windows 黑体，其次 Mac 苹方。
fn install_cjk_fonts(ctx: &egui::Context) {
    let candidates = [
        "C:/Windows/Fonts/msyh.ttc",   // 微软雅黑
        "C:/Windows/Fonts/simhei.ttf", // 黑体
        "/System/Library/Fonts/PingFang.ttc",
        "/System/Library/Fonts/STHeiti Light.ttc",
    ];
    for path in candidates {
        if let Ok(bytes) = std::fs::read(path) {
            let mut fonts = egui::FontDefinitions::default();
            fonts
                .font_data
                .insert("cjk".to_owned(), egui::FontData::from_owned(bytes));
            fonts
                .families
                .entry(egui::FontFamily::Proportional)
                .or_default()
                .insert(0, "cjk".to_owned());
            fonts
                .families
                .entry(egui::FontFamily::Monospace)
                .or_default()
                .push("cjk".to_owned());
            ctx.set_fonts(fonts);
            return;
        }
    }
    log::warn!("没找到中文字体，中文可能显示为方块");
}
