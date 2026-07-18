//! 音频引擎（独立线程）。
//!
//! 信号流：
//! ```text
//!   真麦(cpal 采集) ──► 环形缓冲 ──► MicSource ┐
//!                                              ├─(rodio 混音)─► 输出设备(CABLE Input) ─► 游戏麦克风
//!   音效文件(热键触发) ─► 每次一条独立 Sink ────┘
//!                                              └─(可选)─► 监听设备(你的耳机)
//! ```
//! 引擎跑在自己的线程里（`spawn()`），UI / 热键线程通过 `AudioCtl` 发命令，
//! 这样即使窗口被遮挡、egui 不刷帧，播放也不受影响。
//!
//! 每次触发音效都新建一条 `Sink`，所以不同音效天然可以同时混播；
//! 同一音效重复触发的行为由 `RepeatMode` 决定（从头重播 / 叠加 / 播停切换）。

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink, Source};
use std::collections::{HashMap, VecDeque};
use std::fs::File;
use std::io::BufReader;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::config::{AppConfig, RepeatMode};

/// 列出所有输出设备名。
pub fn output_devices() -> Vec<String> {
    let host = cpal::default_host();
    host.output_devices()
        .map(|ds| ds.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

/// 列出所有输入（录音）设备名。
pub fn input_devices() -> Vec<String> {
    let host = cpal::default_host();
    host.input_devices()
        .map(|ds| ds.filter_map(|d| d.name().ok()).collect())
        .unwrap_or_default()
}

/// 有没有检测到 VB-CABLE（按设备名里含 "CABLE Input" 判断）。
pub fn vbcable_installed() -> bool {
    output_devices().iter().any(|n| n.contains("CABLE Input"))
}

/// 猜一个默认应该选的输出设备名：优先 VB-CABLE 的 CABLE Input。
pub fn guess_cable_output() -> Option<String> {
    output_devices().into_iter().find(|n| n.contains("CABLE Input"))
}

fn find_output_device(name: Option<&str>) -> Result<cpal::Device> {
    let host = cpal::default_host();
    if let Some(name) = name {
        if let Ok(mut it) = host.output_devices() {
            if let Some(d) = it.find(|d| d.name().map(|n| n == name).unwrap_or(false)) {
                return Ok(d);
            }
        }
        log::warn!("找不到输出设备「{name}」，退回系统默认");
    }
    host.default_output_device()
        .ok_or_else(|| anyhow!("没有可用的输出设备"))
}

fn find_input_device(name: Option<&str>) -> Result<cpal::Device> {
    let host = cpal::default_host();
    if let Some(name) = name {
        if let Ok(mut it) = host.input_devices() {
            if let Some(d) = it.find(|d| d.name().map(|n| n == name).unwrap_or(false)) {
                return Ok(d);
            }
        }
        log::warn!("找不到输入设备「{name}」，退回系统默认");
    }
    host.default_input_device()
        .ok_or_else(|| anyhow!("没有可用的麦克风"))
}

/// 一个永不结束的音源：不停从缓冲里取麦克风样本，缓冲空时输出静音（保持流活着）。
struct MicSource {
    buf: Arc<Mutex<VecDeque<f32>>>,
    channels: u16,
    sample_rate: u32,
    enabled: Arc<AtomicBool>,
}

impl Iterator for MicSource {
    type Item = f32;
    fn next(&mut self) -> Option<f32> {
        let s = self.buf.lock().map(|mut b| b.pop_front()).ok().flatten().unwrap_or(0.0);
        if self.enabled.load(Ordering::Relaxed) {
            Some(s)
        } else {
            Some(0.0)
        }
    }
}

impl Source for MicSource {
    fn current_frame_len(&self) -> Option<usize> {
        None
    }
    fn channels(&self) -> u16 {
        self.channels
    }
    fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    fn total_duration(&self) -> Option<Duration> {
        None
    }
}

/// 发给音频线程的命令。
pub enum AudioCmd {
    Play { path: PathBuf, volume: f32 },
    StopAll,
    SetMicPassthrough(bool),
    SetEffectVolume(f32),
    SetRepeatMode(RepeatMode),
    /// 设备等配置变了，整个重建引擎。
    Rebuild(AppConfig),
}

/// 音频线程的控制柄（UI / 热键线程都拿一份克隆）。
#[derive(Clone)]
pub struct AudioCtl {
    tx: Sender<AudioCmd>,
    error: Arc<Mutex<Option<String>>>,
}

impl AudioCtl {
    pub fn send(&self, cmd: AudioCmd) {
        let _ = self.tx.send(cmd);
    }

    /// 引擎最近一次构建的错误（None = 正常）。
    pub fn last_error(&self) -> Option<String> {
        self.error.lock().ok().and_then(|e| e.clone())
    }
}

/// 启动音频线程，返回控制柄。
pub fn spawn(cfg: AppConfig) -> AudioCtl {
    let (tx, rx) = mpsc::channel();
    let error = Arc::new(Mutex::new(None));
    let error2 = error.clone();
    let _ = std::thread::Builder::new()
        .name("audio".into())
        .spawn(move || audio_thread(cfg, rx, error2));
    AudioCtl { tx, error }
}

fn set_error(slot: &Arc<Mutex<Option<String>>>, e: Option<String>) {
    if let Ok(mut s) = slot.lock() {
        *s = e;
    }
}

fn build_engine(cfg: &AppConfig, error: &Arc<Mutex<Option<String>>>) -> Option<AudioEngine> {
    match AudioEngine::new(cfg) {
        Ok(e) => {
            set_error(error, None);
            Some(e)
        }
        Err(e) => {
            set_error(error, Some(format!("{e:#}")));
            None
        }
    }
}

fn audio_thread(mut cfg: AppConfig, rx: Receiver<AudioCmd>, error: Arc<Mutex<Option<String>>>) {
    let mut engine = build_engine(&cfg, &error);
    loop {
        // 带超时的等待：空闲时也定期醒来清理已放完的 Sink。
        match rx.recv_timeout(Duration::from_millis(500)) {
            Ok(cmd) => match cmd {
                AudioCmd::Play { path, volume } => {
                    if let Some(en) = &mut engine {
                        en.play(&path, volume, cfg.repeat_mode);
                    }
                }
                AudioCmd::StopAll => {
                    if let Some(en) = &mut engine {
                        en.stop_all();
                    }
                }
                AudioCmd::SetMicPassthrough(on) => {
                    cfg.mic_passthrough = on;
                    if let Some(en) = &engine {
                        en.set_mic_passthrough(on);
                    }
                }
                AudioCmd::SetEffectVolume(v) => {
                    cfg.effect_volume = v;
                    if let Some(en) = &mut engine {
                        en.set_effect_volume(v);
                    }
                }
                AudioCmd::SetRepeatMode(m) => cfg.repeat_mode = m,
                AudioCmd::Rebuild(c) => {
                    cfg = c;
                    drop(engine.take()); // 先释放旧设备，再开新的
                    engine = build_engine(&cfg, &error);
                }
            },
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => return, // UI 退出
        }
        if let Some(en) = &mut engine {
            en.reap();
        }
    }
}

/// 一条正在播放的音轨：主输出 Sink + 可选监听 Sink。
struct Voice {
    sink: Sink,
    mon: Option<Sink>,
    /// 该音效自身的音量倍率（不含全局音量，全局变化时用它重算）。
    base: f32,
}

impl Voice {
    fn stop(&self) {
        self.sink.stop();
        if let Some(m) = &self.mon {
            m.stop();
        }
    }
    fn finished(&self) -> bool {
        self.sink.empty() && self.mon.as_ref().map(|m| m.empty()).unwrap_or(true)
    }
}

struct AudioEngine {
    // 主输出（CABLE）：拿住 OutputStream 不能 drop，否则声音停。
    _out_stream: OutputStream,
    out_handle: OutputStreamHandle,
    // 监听（耳机），可选
    _mon_stream: Option<OutputStream>,
    mon_handle: Option<OutputStreamHandle>,
    // 麦克风采集流，拿住不能 drop
    _mic_stream: Option<cpal::Stream>,
    mic_enabled: Arc<AtomicBool>,
    effect_volume: f32,
    /// 按音频文件分组的活跃音轨（同一文件可有多条 = 叠加播放）。
    voices: HashMap<PathBuf, Vec<Voice>>,
}

impl AudioEngine {
    /// 按当前配置搭好整条音频链路。
    fn new(cfg: &AppConfig) -> Result<Self> {
        // —— 主输出到 CABLE ——
        let out_device = find_output_device(cfg.output_device.as_deref())?;
        let (out_stream, out_handle) = OutputStream::try_from_device(&out_device)
            .context("打开输出设备失败（该设备可能被独占占用）")?;

        // —— 可选监听设备 ——
        let (mon_stream, mon_handle) = match cfg.monitor_device.as_deref() {
            Some(name) => match find_output_device(Some(name))
                .and_then(|d| OutputStream::try_from_device(&d).map_err(Into::into))
            {
                Ok((s, h)) => (Some(s), Some(h)),
                Err(e) => {
                    log::warn!("监听设备打开失败：{e}");
                    (None, None)
                }
            },
            None => (None, None),
        };

        // —— 麦克风采集 + 转发 ——
        let mic_enabled = Arc::new(AtomicBool::new(cfg.mic_passthrough));
        let mic_stream = match Self::start_mic(cfg.input_device.as_deref(), &out_handle, mic_enabled.clone()) {
            Ok(stream) => Some(stream),
            Err(e) => {
                log::warn!("麦克风转发未启动：{e}");
                None
            }
        };

        Ok(Self {
            _out_stream: out_stream,
            out_handle,
            _mon_stream: mon_stream,
            mon_handle,
            _mic_stream: mic_stream,
            mic_enabled,
            effect_volume: cfg.effect_volume,
            voices: HashMap::new(),
        })
    }

    /// 开一条麦克风采集流，把样本喂进缓冲，并让 rodio 播放对应的 MicSource。
    fn start_mic(
        name: Option<&str>,
        out_handle: &OutputStreamHandle,
        enabled: Arc<AtomicBool>,
    ) -> Result<cpal::Stream> {
        let device = find_input_device(name)?;
        let supported = device
            .default_input_config()
            .context("读取麦克风默认参数失败")?;
        let channels = supported.channels();
        let sample_rate = supported.sample_rate().0;
        let sample_format = supported.sample_format();
        let config: cpal::StreamConfig = supported.into();

        // 缓冲上限 ~150ms，超了丢最旧的样本，控制延迟。
        let cap = (sample_rate as usize * channels as usize * 150) / 1000;
        let buf: Arc<Mutex<VecDeque<f32>>> = Arc::new(Mutex::new(VecDeque::with_capacity(cap * 2)));
        let buf_producer = buf.clone();

        let err_fn = |e| log::error!("麦克风流错误：{e}");
        let push = move |samples: &[f32]| {
            if let Ok(mut b) = buf_producer.lock() {
                b.extend(samples.iter().copied());
                while b.len() > cap {
                    b.pop_front();
                }
            }
        };

        let stream = match sample_format {
            cpal::SampleFormat::F32 => {
                let push = push.clone();
                device.build_input_stream(
                    &config,
                    move |data: &[f32], _| push(data),
                    err_fn,
                    None,
                )?
            }
            cpal::SampleFormat::I16 => {
                let push = push.clone();
                device.build_input_stream(
                    &config,
                    move |data: &[i16], _| {
                        let f: Vec<f32> = data.iter().map(|s| *s as f32 / i16::MAX as f32).collect();
                        push(&f);
                    },
                    err_fn,
                    None,
                )?
            }
            cpal::SampleFormat::U16 => {
                let push = push.clone();
                device.build_input_stream(
                    &config,
                    move |data: &[u16], _| {
                        let f: Vec<f32> = data
                            .iter()
                            .map(|s| (*s as f32 / u16::MAX as f32) * 2.0 - 1.0)
                            .collect();
                        push(&f);
                    },
                    err_fn,
                    None,
                )?
            }
            other => return Err(anyhow!("不支持的麦克风采样格式：{other:?}")),
        };

        stream.play().context("启动麦克风流失败")?;

        let mic_source = MicSource {
            buf,
            channels,
            sample_rate,
            enabled,
        };
        out_handle
            .play_raw(mic_source)
            .context("把麦克风接入输出失败")?;

        Ok(stream)
    }

    /// 触发一个音效。`mode` 决定同一音效已在播时的行为。
    fn play(&mut self, path: &Path, volume: f32, mode: RepeatMode) {
        self.reap();
        let playing = self.voices.get(path).map(|v| !v.is_empty()).unwrap_or(false);
        match mode {
            // 播/停切换：已在播 -> 停掉并返回
            RepeatMode::Toggle if playing => {
                if let Some(vs) = self.voices.remove(path) {
                    for v in &vs {
                        v.stop();
                    }
                }
                return;
            }
            // 从头重播：先停掉旧的
            RepeatMode::Restart if playing => {
                if let Some(vs) = self.voices.remove(path) {
                    for v in &vs {
                        v.stop();
                    }
                }
            }
            // 叠加播放：什么都不用做，直接再开一条
            _ => {}
        }

        let src = match Self::decode(path) {
            Ok(s) => s,
            Err(e) => {
                log::error!("解码音频失败 {}：{e}", path.display());
                return;
            }
        };
        let sink = match Sink::try_new(&self.out_handle) {
            Ok(s) => s,
            Err(e) => {
                log::error!("创建播放通道失败：{e}");
                return;
            }
        };
        sink.set_volume(self.effect_volume * volume);
        sink.append(src);

        let mon = self.mon_handle.as_ref().and_then(|h| {
            let s = Sink::try_new(h).ok()?;
            let src = Self::decode(path).ok()?;
            s.set_volume(self.effect_volume * volume);
            s.append(src);
            Some(s)
        });

        self.voices
            .entry(path.to_path_buf())
            .or_default()
            .push(Voice { sink, mon, base: volume });
    }

    fn decode(path: &Path) -> Result<Decoder<BufReader<File>>> {
        let file = File::open(path).with_context(|| format!("打不开 {}", path.display()))?;
        Decoder::new(BufReader::new(file)).map_err(|e| anyhow!("{e}"))
    }

    /// 停止所有正在播放的音效。
    fn stop_all(&mut self) {
        for (_, vs) in self.voices.drain() {
            for v in &vs {
                v.stop();
            }
        }
    }

    /// 清理已经播完的音轨（释放 Sink）。
    fn reap(&mut self) {
        self.voices.retain(|_, vs| {
            vs.retain(|v| !v.finished());
            !vs.is_empty()
        });
    }

    /// 运行时开关麦克风转发（无需重建引擎）。
    fn set_mic_passthrough(&self, on: bool) {
        self.mic_enabled.store(on, Ordering::Relaxed);
    }

    /// 运行时调全局音效音量（同步作用到正在播的音轨）。
    fn set_effect_volume(&mut self, v: f32) {
        self.effect_volume = v;
        for vs in self.voices.values() {
            for voice in vs {
                voice.sink.set_volume(v * voice.base);
                if let Some(m) = &voice.mon {
                    m.set_volume(v * voice.base);
                }
            }
        }
    }
}
