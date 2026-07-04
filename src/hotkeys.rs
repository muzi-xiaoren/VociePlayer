//! 全局热键：把「Ctrl+Alt+S」这样的字符串注册成系统级快捷键。
//!
//! 快捷键字符串由界面里「按键捕获」生成（见 `from_egui`），也可手改。
//! 我们自己在字符串 <-> `global_hotkey::HotKey` 之间转换，不依赖第三方解析格式。

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyManager, GlobalHotKeyEvent, HotKeyState};
use std::collections::HashMap;
use std::path::PathBuf;

/// 热键触发后要做的事。
#[derive(Debug, Clone)]
pub enum HkAction {
    Play { path: PathBuf, volume: f32 },
    StopAll,
}

/// 把界面捕获到的（修饰键 + 主键）拼成展示字符串，如 "Ctrl+Alt+S"。
/// 返回 None 表示这个键我们不支持绑定。
pub fn from_egui(mods: &egui::Modifiers, key: egui::Key) -> Option<String> {
    let token = egui_key_token(key)?;
    let mut parts = Vec::new();
    if mods.ctrl || mods.command {
        parts.push("Ctrl");
    }
    if mods.alt {
        parts.push("Alt");
    }
    if mods.shift {
        parts.push("Shift");
    }
    let mut s = parts.join("+");
    if !s.is_empty() {
        s.push('+');
    }
    s.push_str(token);
    Some(s)
}

/// 解析 "Ctrl+Alt+S" -> HotKey。失败返回 None。
pub fn parse(combo: &str) -> Option<HotKey> {
    let mut mods = Modifiers::empty();
    let mut code: Option<Code> = None;
    for part in combo.split('+').map(|p| p.trim()).filter(|p| !p.is_empty()) {
        match part.to_lowercase().as_str() {
            "ctrl" | "control" => mods |= Modifiers::CONTROL,
            "alt" | "option" => mods |= Modifiers::ALT,
            "shift" => mods |= Modifiers::SHIFT,
            // Win/Super/Cmd 键暂不支持绑定，忽略这个修饰符
            "win" | "super" | "meta" | "cmd" => {}
            _ => code = token_to_code(part),
        }
    }
    let code = code?;
    let m = if mods.is_empty() { None } else { Some(mods) };
    Some(HotKey::new(m, code))
}

fn egui_key_token(key: egui::Key) -> Option<&'static str> {
    use egui::Key::*;
    Some(match key {
        A => "A", B => "B", C => "C", D => "D", E => "E", F => "F", G => "G",
        H => "H", I => "I", J => "J", K => "K", L => "L", M => "M", N => "N",
        O => "O", P => "P", Q => "Q", R => "R", S => "S", T => "T", U => "U",
        V => "V", W => "W", X => "X", Y => "Y", Z => "Z",
        Num0 => "0", Num1 => "1", Num2 => "2", Num3 => "3", Num4 => "4",
        Num5 => "5", Num6 => "6", Num7 => "7", Num8 => "8", Num9 => "9",
        F1 => "F1", F2 => "F2", F3 => "F3", F4 => "F4", F5 => "F5", F6 => "F6",
        F7 => "F7", F8 => "F8", F9 => "F9", F10 => "F10", F11 => "F11", F12 => "F12",
        Space => "Space", Enter => "Enter", Tab => "Tab", Backspace => "Backspace",
        Insert => "Insert", Delete => "Delete", Home => "Home", End => "End",
        PageUp => "PageUp", PageDown => "PageDown",
        ArrowUp => "Up", ArrowDown => "Down", ArrowLeft => "Left", ArrowRight => "Right",
        _ => return None,
    })
}

fn token_to_code(t: &str) -> Option<Code> {
    Some(match t.to_uppercase().as_str() {
        "A" => Code::KeyA, "B" => Code::KeyB, "C" => Code::KeyC, "D" => Code::KeyD,
        "E" => Code::KeyE, "F" => Code::KeyF, "G" => Code::KeyG, "H" => Code::KeyH,
        "I" => Code::KeyI, "J" => Code::KeyJ, "K" => Code::KeyK, "L" => Code::KeyL,
        "M" => Code::KeyM, "N" => Code::KeyN, "O" => Code::KeyO, "P" => Code::KeyP,
        "Q" => Code::KeyQ, "R" => Code::KeyR, "S" => Code::KeyS, "T" => Code::KeyT,
        "U" => Code::KeyU, "V" => Code::KeyV, "W" => Code::KeyW, "X" => Code::KeyX,
        "Y" => Code::KeyY, "Z" => Code::KeyZ,
        "0" => Code::Digit0, "1" => Code::Digit1, "2" => Code::Digit2, "3" => Code::Digit3,
        "4" => Code::Digit4, "5" => Code::Digit5, "6" => Code::Digit6, "7" => Code::Digit7,
        "8" => Code::Digit8, "9" => Code::Digit9,
        "F1" => Code::F1, "F2" => Code::F2, "F3" => Code::F3, "F4" => Code::F4,
        "F5" => Code::F5, "F6" => Code::F6, "F7" => Code::F7, "F8" => Code::F8,
        "F9" => Code::F9, "F10" => Code::F10, "F11" => Code::F11, "F12" => Code::F12,
        "SPACE" => Code::Space, "ENTER" => Code::Enter, "TAB" => Code::Tab,
        "BACKSPACE" => Code::Backspace, "INSERT" => Code::Insert, "DELETE" => Code::Delete,
        "HOME" => Code::Home, "END" => Code::End, "PAGEUP" => Code::PageUp,
        "PAGEDOWN" => Code::PageDown, "UP" => Code::ArrowUp, "DOWN" => Code::ArrowDown,
        "LEFT" => Code::ArrowLeft, "RIGHT" => Code::ArrowRight,
        _ => return None,
    })
}

/// 热键管理：注册一批快捷键，维护「热键 id -> 动作」的映射。
pub struct Hotkeys {
    mgr: GlobalHotKeyManager,
    current: Vec<HotKey>,
    actions: HashMap<u32, HkAction>,
}

impl Hotkeys {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            mgr: GlobalHotKeyManager::new().map_err(|e| anyhow::anyhow!("{e}"))?,
            current: Vec::new(),
            actions: HashMap::new(),
        })
    }

    /// 注销当前所有热键。
    pub fn clear(&mut self) {
        if !self.current.is_empty() {
            let _ = self.mgr.unregister_all(&self.current);
        }
        self.current.clear();
        self.actions.clear();
    }

    /// 注册一个快捷键并绑定动作。返回是否成功（快捷键非法或被占用会失败）。
    pub fn register(&mut self, combo: &str, action: HkAction) -> bool {
        let Some(hk) = parse(combo) else {
            log::warn!("无法解析快捷键「{combo}」");
            return false;
        };
        match self.mgr.register(hk) {
            Ok(()) => {
                self.current.push(hk);
                self.actions.insert(hk.id(), action);
                true
            }
            Err(e) => {
                log::warn!("注册快捷键「{combo}」失败（可能被其它程序占用）：{e}");
                false
            }
        }
    }

    /// 取出所有「本次刚被按下」的动作。在 UI 主循环里轮询调用。
    pub fn poll(&self) -> Vec<HkAction> {
        let mut out = Vec::new();
        while let Ok(ev) = GlobalHotKeyEvent::receiver().try_recv() {
            if ev.state == HotKeyState::Pressed {
                if let Some(a) = self.actions.get(&ev.id) {
                    out.push(a.clone());
                }
            }
        }
        out
    }
}
