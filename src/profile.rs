//! Profile（配置组）= profiles 下的一个子文件夹。
//!
//! 文件夹里的音频文件会被自动扫描进列表；每个音频的快捷键 / 单独音量
//! 记在同目录的 `_bindings.json` 里（以文件名为键），移动/改名音频不丢绑定。

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// 支持的音频扩展名（小写）。
const AUDIO_EXTS: &[&str] = &["mp3", "wav", "ogg", "flac", "m4a", "aac"];

/// 一个音效条目。
#[derive(Debug, Clone)]
pub struct Sound {
    /// 展示名（文件名去扩展名）。
    pub name: String,
    /// 音频文件绝对路径。
    pub path: PathBuf,
    /// 绑定的快捷键，如 "Ctrl+Alt+S"；None = 未绑定。
    pub hotkey: Option<String>,
    /// 单独音量倍率（默认 1.0）。
    pub volume: f32,
}

/// 持久化到 `_bindings.json` 的单条绑定信息。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Binding {
    #[serde(default)]
    hotkey: Option<String>,
    #[serde(default = "default_volume")]
    volume: f32,
}

fn default_volume() -> f32 {
    1.0
}

/// 一个 profile。
#[derive(Debug, Clone)]
pub struct Profile {
    pub name: String,
    pub dir: PathBuf,
    pub sounds: Vec<Sound>,
}

fn is_audio(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .map(|e| AUDIO_EXTS.contains(&e.to_lowercase().as_str()))
        .unwrap_or(false)
}

/// 列出所有 profile 名（profiles 下的子文件夹）。若一个都没有，创建「默认」。
pub fn list_profiles() -> Vec<String> {
    let dir = crate::config::profiles_dir();
    let _ = std::fs::create_dir_all(&dir);
    let mut names: Vec<String> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.filter_map(|e| e.ok())
                .filter(|e| e.path().is_dir())
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    if names.is_empty() {
        let _ = std::fs::create_dir_all(dir.join("默认"));
        names.push("默认".to_string());
    }
    names.sort();
    names
}

/// 新建一个 profile 文件夹。
pub fn create_profile(name: &str) -> std::io::Result<()> {
    std::fs::create_dir_all(crate::config::profiles_dir().join(name))
}

impl Profile {
    fn bindings_file(dir: &Path) -> PathBuf {
        dir.join("_bindings.json")
    }

    /// 扫描文件夹、合并 `_bindings.json`，加载出 profile。
    pub fn load(name: &str) -> Self {
        let dir = crate::config::profiles_dir().join(name);
        let _ = std::fs::create_dir_all(&dir);

        // 读已有绑定
        let bindings: BTreeMap<String, Binding> = std::fs::read_to_string(Self::bindings_file(&dir))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default();

        // 扫描音频文件
        let mut sounds: Vec<Sound> = std::fs::read_dir(&dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.is_file() && is_audio(p))
                    .map(|p| {
                        let file_name = p
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or_default()
                            .to_string();
                        let name = p
                            .file_stem()
                            .and_then(|n| n.to_str())
                            .unwrap_or(&file_name)
                            .to_string();
                        let b = bindings.get(&file_name);
                        Sound {
                            name,
                            path: p,
                            hotkey: b.and_then(|b| b.hotkey.clone()),
                            volume: b.map(|b| b.volume).unwrap_or(1.0),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        sounds.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));

        Profile {
            name: name.to_string(),
            dir,
            sounds,
        }
    }

    /// 把当前绑定写回 `_bindings.json`。
    pub fn save_bindings(&self) {
        let mut map: BTreeMap<String, Binding> = BTreeMap::new();
        for s in &self.sounds {
            if let Some(fname) = s.path.file_name().and_then(|n| n.to_str()) {
                map.insert(
                    fname.to_string(),
                    Binding {
                        hotkey: s.hotkey.clone(),
                        volume: s.volume,
                    },
                );
            }
        }
        if let Ok(json) = serde_json::to_string_pretty(&map) {
            let _ = std::fs::write(Self::bindings_file(&self.dir), json);
        }
    }

    /// 当前文件夹里的音频文件名集合（用于「有没有新文件」的轻量比对）。
    pub fn file_signature(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok())
                    .map(|e| e.path())
                    .filter(|p| p.is_file() && is_audio(p))
                    .filter_map(|p| p.file_name().and_then(|n| n.to_str()).map(String::from))
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }
}
