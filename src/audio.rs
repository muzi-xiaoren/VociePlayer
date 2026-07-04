//! 音频引擎。
//!
//! 信号流：
//! ```text
//!   真麦(cpal 采集) ──► 环形缓冲 ──► MicSource ┐
//!                                              ├─(rodio 混音)─► 输出设备(CABLE Input) ─► 游戏麦克风
//!   音效文件(热键触发) ─► rodio Decoder ────────┘
//!                                              └─(可选)─► 监听设备(你的耳机)
//! ```
//! rodio 负责音效解码、重采样、和麦克风一起混音后送到输出设备；麦克风的实时采集
//! 用 cpal，采集到的样本丢进一个有上限的缓冲，由一个「永不结束」的 MicSource 取出播放。

use anyhow::{anyhow, Context, Result};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use rodio::{Decoder, OutputStream, OutputStreamHandle, Sink, Source};
use std::collections::VecDeque;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

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

pub struct AudioEngine {
    // 主输出（CABLE）：拿住 OutputStream 不能 drop，否则声音停。
    _out_stream: OutputStream,
    effect_sink: Sink,
    // 监听（耳机），可选
    _mon_stream: Option<OutputStream>,
    mon_sink: Option<Sink>,
    // 麦克风采集流，拿住不能 drop
    _mic_stream: Option<cpal::Stream>,
    mic_enabled: Arc<AtomicBool>,
    effect_volume: f32,
}

impl AudioEngine {
    /// 按当前配置搭好整条音频链路。
    pub fn new(cfg: &crate::config::AppConfig) -> Result<Self> {
        // —— 主输出到 CABLE ——
        let out_device = find_output_device(cfg.output_device.as_deref())?;
        let (out_stream, out_handle) = OutputStream::try_from_device(&out_device)
            .context("打开输出设备失败（该设备可能被独占占用）")?;
        let effect_sink = Sink::try_new(&out_handle).context("创建音效播放器失败")?;
        effect_sink.set_volume(cfg.effect_volume);

        // —— 可选监听设备 ——
        let (mon_stream, mon_sink) = match cfg.monitor_device.as_deref() {
            Some(name) => match find_output_device(Some(name))
                .and_then(|d| OutputStream::try_from_device(&d).map_err(Into::into))
            {
                Ok((s, h)) => {
                    let sink = Sink::try_new(&h).ok();
                    if let Some(sk) = &sink {
                        sk.set_volume(cfg.effect_volume);
                    }
                    (Some(s), sink)
                }
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
            effect_sink,
            _mon_stream: mon_stream,
            mon_sink,
            _mic_stream: mic_stream,
            mic_enabled,
            effect_volume: cfg.effect_volume,
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

    /// 触发一个音效：混进输出（CABLE），并在监听设备上也放一份。
    pub fn play(&self, path: &Path, volume: f32) {
        let vol = volume * self.effect_volume;
        match Self::decode(path) {
            Ok(src) => self.effect_sink.append(src.amplify(vol)),
            Err(e) => log::error!("解码音频失败 {}：{e}", path.display()),
        }
        if let Some(mon) = &self.mon_sink {
            if let Ok(src) = Self::decode(path) {
                mon.append(src.amplify(vol));
            }
        }
    }

    fn decode(path: &Path) -> Result<Decoder<BufReader<File>>> {
        let file = File::open(path).with_context(|| format!("打不开 {}", path.display()))?;
        Decoder::new(BufReader::new(file)).map_err(|e| anyhow!("{e}"))
    }

    /// 停止所有正在播放的音效。
    pub fn stop_all(&self) {
        self.effect_sink.stop();
        if let Some(mon) = &self.mon_sink {
            mon.stop();
        }
    }

    /// 运行时开关麦克风转发（无需重建引擎）。
    pub fn set_mic_passthrough(&self, on: bool) {
        self.mic_enabled.store(on, Ordering::Relaxed);
    }

    /// 运行时调音效音量。
    pub fn set_effect_volume(&mut self, v: f32) {
        self.effect_volume = v;
        self.effect_sink.set_volume(v);
        if let Some(mon) = &self.mon_sink {
            mon.set_volume(v);
        }
    }
}
