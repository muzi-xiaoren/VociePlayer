//! egui 界面 + 状态管理，把配置、音频线程、热键、profile 串起来。
//!
//! 音频跑在独立线程（见 `audio::spawn`），热键监听也在独立线程
//! （见 `hotkeys::spawn_listener`），UI 只负责发命令 —— 窗口被游戏
//! 遮挡、最小化时播放照常工作。

use crate::audio::{self, AudioCmd, AudioCtl};
use crate::config::{AppConfig, RepeatMode};
use crate::hotkeys::{self, HkAction, Hotkeys};
use crate::platform;
use crate::profile::{self, Profile, Sound};
use std::path::PathBuf;
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
    /// 弹出「选择文件夹」原生对话框，把选中的文件夹加成外部配置。
    pick_folder: bool,
    /// 移除一个外部文件夹配置（不删文件）。
    remove_external: Option<String>,
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
    audio: AudioCtl,
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

        // 确定当前 profile（内置 + 外部文件夹都算）。
        let profiles = Self::all_profiles(&config);
        let active = config
            .active_profile
            .clone()
            .filter(|n| profiles.contains(n))
            .or_else(|| profiles.first().cloned());
        config.active_profile = active.clone();
        let profile = active
            .as_deref()
            .map(|n| Profile::load(n, &Self::dir_for_config(&config, n)));
        let last_signature = profile
            .as_ref()
            .map(|p| Profile::file_signature(&p.dir))
            .unwrap_or_default();

        // 音频线程
        let audio = audio::spawn(config.clone());

        // 全局热键：manager 在主线程建；事件监听放独立线程，直接驱动音频线程。
        let hotkeys = match Hotkeys::new() {
            Ok(h) => Some(h),
            Err(e) => {
                log::error!("初始化全局热键失败：{e}");
                None
            }
        };
        if let Some(hk) = &hotkeys {
            let audio2 = audio.clone();
            hotkeys::spawn_listener(hk.actions_handle(), move |a| match a {
                HkAction::Play { path, volume } => audio2.send(AudioCmd::Play { path, volume }),
                HkAction::StopAll => audio2.send(AudioCmd::StopAll),
            });
        }

        let mut app = Self {
            config,
            audio,
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
        app.reregister_hotkeys();
        app
    }

    /// 全部 profile 名：profiles 目录下的子文件夹 + 配置里记的外部文件夹。
    fn all_profiles(cfg: &AppConfig) -> Vec<String> {
        let mut names = profile::list_profiles();
        for n in cfg.external_profiles.keys() {
            if !names.contains(n) {
                names.push(n.clone());
            }
        }
        names.sort();
        names
    }

    /// profile 名 -> 实际文件夹（外部配置优先）。
    fn dir_for_config(cfg: &AppConfig, name: &str) -> PathBuf {
        cfg.external_profiles
            .get(name)
            .cloned()
            .unwrap_or_else(|| crate::config::profiles_dir().join(name))
    }

    fn dir_for(&self, name: &str) -> PathBuf {
        Self::dir_for_config(&self.config, name)
    }

    fn rebuild_engine(&mut self) {
        self.audio.send(AudioCmd::Rebuild(self.config.clone()));
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
        let dir = self.dir_for(name);
        let p = Profile::load(name, &dir);
        self.last_signature = Profile::file_signature(&p.dir);
        self.profile = Some(p);
        self.config.save();
        self.reregister_hotkeys();
    }

    /// 把任意文件夹加成一个外部配置并切过去。名字取文件夹名，重名自动加序号。
    fn add_external_folder(&mut self, dir: PathBuf) {
        // 同一个文件夹已经加过：直接切过去
        if let Some(name) = self
            .config
            .external_profiles
            .iter()
            .find(|(_, d)| **d == dir)
            .map(|(n, _)| n.clone())
        {
            self.switch_profile(&name);
            return;
        }
        let base = dir
            .file_name()
            .and_then(|n| n.to_str())
            .map(String::from)
            .unwrap_or_else(|| dir.display().to_string());
        let mut name = base.clone();
        let mut i = 2;
        while self.profiles.contains(&name) {
            name = format!("{base} ({i})");
            i += 1;
        }
        self.config.external_profiles.insert(name.clone(), dir);
        self.config.save();
        self.profiles = Self::all_profiles(&self.config);
        self.switch_profile(&name);
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
                self.profile = Some(Profile::load(&name, &dir));
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
            if let Some(p) = &self.profile {
                if let Some(s) = p.sounds.get(i) {
                    self.audio.send(AudioCmd::Play {
                        path: s.path.clone(),
                        volume: s.volume,
                    });
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

        // 「新建配置」：普通名字 = profiles 下新建子文件夹；
        // 粘贴的是绝对路径 = 当成外部文件夹加进来。
        if let Some(input) = pending.new_profile {
            let input = input.trim().to_string();
            if !input.is_empty() {
                let p = PathBuf::from(&input);
                if p.is_absolute() {
                    if p.is_dir() || std::fs::create_dir_all(&p).is_ok() {
                        self.add_external_folder(p);
                        self.new_profile_name.clear();
                    }
                } else if !input.contains('/')
                    && !input.contains('\\')
                    && profile::create_profile(&input).is_ok()
                {
                    self.profiles = Self::all_profiles(&self.config);
                    self.switch_profile(&input);
                    self.new_profile_name.clear();
                }
            }
        }

        // 原生「选择文件夹」对话框（有确定按钮）。
        if pending.pick_folder {
            if let Some(dir) = rfd::FileDialog::new()
                .set_title("选择音效文件夹")
                .pick_folder()
            {
                self.add_external_folder(dir);
            }
        }

        if let Some(name) = pending.remove_external {
            self.config.external_profiles.remove(&name);
            self.profiles = Self::all_profiles(&self.config);
            if self.config.active_profile.as_deref() == Some(name.as_str()) {
                if let Some(first) = self.profiles.first().cloned() {
                    self.switch_profile(&first);
                } else {
                    self.config.active_profile = None;
                    self.profile = None;
                    self.config.save();
                    need_reregister = true;
                }
            } else {
                self.config.save();
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
            self.audio
                .send(AudioCmd::SetMicPassthrough(self.config.mic_passthrough));
            self.config.save();
        }

        // —— 音效音量 ——
        ui.horizontal(|ui| {
            ui.label("音效音量");
            let resp = ui.add(egui::Slider::new(&mut self.config.effect_volume, 0.0..=1.5));
            if resp.changed() {
                self.audio
                    .send(AudioCmd::SetEffectVolume(self.config.effect_volume));
                self.config.save();
            }
        });

        // —— 重复按键行为 ——
        ui.horizontal(|ui| {
            ui.label("重复按同一键：");
            let mut changed = false;
            changed |= ui
                .selectable_value(&mut self.config.repeat_mode, RepeatMode::Restart, "从头重播")
                .clicked();
            changed |= ui
                .selectable_value(&mut self.config.repeat_mode, RepeatMode::Overlap, "叠加再播")
                .clicked();
            changed |= ui
                .selectable_value(&mut self.config.repeat_mode, RepeatMode::Toggle, "一按播 / 再按停")
                .clicked();
            if changed {
                self.audio.send(AudioCmd::SetRepeatMode(self.config.repeat_mode));
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
        if let Some(err) = self.audio.last_error() {
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
                        let label = if self.config.external_profiles.contains_key(name) {
                            format!("📁 {name}")
                        } else {
                            name.clone()
                        };
                        if ui.selectable_label(selected, label).clicked() && !selected {
                            pending.switch_profile = Some(name.clone());
                        }
                    }
                });
            if ui.button("📂 打开文件夹").clicked() {
                pending.open_folder = true;
            }
            // 外部文件夹配置可以移除（只解除关联，不删文件）
            let is_external = self
                .config
                .active_profile
                .as_ref()
                .map(|n| self.config.external_profiles.contains_key(n))
                .unwrap_or(false);
            if is_external
                && ui
                    .button("✖ 移除")
                    .on_hover_text("从列表移除这个外部文件夹（不删除文件）")
                    .clicked()
            {
                pending.remove_external = self.config.active_profile.clone();
            }
        });
        ui.horizontal(|ui| {
            ui.label("新建配置：");
            ui.text_edit_singleline(&mut self.new_profile_name)
                .on_hover_text("输入名字新建；也可以直接粘贴一个文件夹的完整路径");
            if ui.button("新建").clicked() {
                pending.new_profile = Some(self.new_profile_name.clone());
            }
            if ui
                .button("📁 选择文件夹…")
                .on_hover_text("把电脑上任意一个装音效的文件夹加成配置")
                .clicked()
            {
                pending.pick_folder = true;
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
        // 热键和播放都在独立线程，这里的定时刷帧只为文件夹自动重扫。
        ctx.request_repaint_after(Duration::from_millis(500));

        // 1) 文件夹自动重扫
        self.maybe_rescan();

        // 2) 快捷键捕获
        self.handle_capture(ctx);

        // 3) 界面（设备/profile 列表先克隆成局部，避开借用冲突）
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

        // 4) 帧末统一处理
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
