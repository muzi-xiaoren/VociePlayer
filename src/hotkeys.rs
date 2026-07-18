//! 全局热键：把「Ctrl+Alt+S」这样的字符串注册成系统级快捷键。
//!
//! 快捷键字符串由界面里「按键捕获」生成（见 `from_egui`），也可手改。
//! 我们自己在字符串 <-> `global_hotkey::HotKey` 之间转换，不依赖第三方解析格式。
//!
//! 事件处理在独立线程（`spawn_listener`）里阻塞等待，不依赖 UI 刷帧——
//! 窗口被全屏游戏遮挡、最小化时热键照样响。数字键会同时注册主键盘和
//! 小键盘两个键位，按哪边都能触发。

use global_hotkey::hotkey::{Code, HotKey, Modifiers};
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

/// 热键触发后要做的事。
#[derive(Debug, Clone)]
pub enum HkAction {
    Play { path: PathBuf, volume: f32 },
    StopAll,
}

/// 「热键 id -> 动作」映射，注册线程（UI）和监听线程共享。
pub type SharedActions = Arc<Mutex<HashMap<u32, HkAction>>>;

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

/// 解析 "Ctrl+Alt+1" -> 一批 HotKey（数字/回车会展开成主键盘+小键盘两个）。
/// 解析失败返回空 Vec。
pub fn parse_all(combo: &str) -> Vec<HotKey> {
    let mut mods = Modifiers::empty();
    let mut codes: Vec<Code> = Vec::new();
    for part in combo.split('+').map(|p| p.trim()).filter(|p| !p.is_empty()) {
        match part.to_lowercase().as_str() {
            "ctrl" | "control" => mods |= Modifiers::CONTROL,
            "alt" | "option" => mods |= Modifiers::ALT,
            "shift" => mods |= Modifiers::SHIFT,
            // Win/Super/Cmd 键暂不支持绑定，忽略这个修饰符
            "win" | "super" | "meta" | "cmd" => {}
            _ => codes = token_to_codes(part),
        }
    }
    let m = if mods.is_empty() { None } else { Some(mods) };
    codes.into_iter().map(|c| HotKey::new(m, c)).collect()
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

/// token -> 键位。数字和回车展开成两个键位（主键盘 + 小键盘）。
fn token_to_codes(t: &str) -> Vec<Code> {
    match t.to_uppercase().as_str() {
        "0" => vec![Code::Digit0, Code::Numpad0],
        "1" => vec![Code::Digit1, Code::Numpad1],
        "2" => vec![Code::Digit2, Code::Numpad2],
        "3" => vec![Code::Digit3, Code::Numpad3],
        "4" => vec![Code::Digit4, Code::Numpad4],
        "5" => vec![Code::Digit5, Code::Numpad5],
        "6" => vec![Code::Digit6, Code::Numpad6],
        "7" => vec![Code::Digit7, Code::Numpad7],
        "8" => vec![Code::Digit8, Code::Numpad8],
        "9" => vec![Code::Digit9, Code::Numpad9],
        "ENTER" => vec![Code::Enter, Code::NumpadEnter],
        "A" => vec![Code::KeyA], "B" => vec![Code::KeyB], "C" => vec![Code::KeyC],
        "D" => vec![Code::KeyD], "E" => vec![Code::KeyE], "F" => vec![Code::KeyF],
        "G" => vec![Code::KeyG], "H" => vec![Code::KeyH], "I" => vec![Code::KeyI],
        "J" => vec![Code::KeyJ], "K" => vec![Code::KeyK], "L" => vec![Code::KeyL],
        "M" => vec![Code::KeyM], "N" => vec![Code::KeyN], "O" => vec![Code::KeyO],
        "P" => vec![Code::KeyP], "Q" => vec![Code::KeyQ], "R" => vec![Code::KeyR],
        "S" => vec![Code::KeyS], "T" => vec![Code::KeyT], "U" => vec![Code::KeyU],
        "V" => vec![Code::KeyV], "W" => vec![Code::KeyW], "X" => vec![Code::KeyX],
        "Y" => vec![Code::KeyY], "Z" => vec![Code::KeyZ],
        "F1" => vec![Code::F1], "F2" => vec![Code::F2], "F3" => vec![Code::F3],
        "F4" => vec![Code::F4], "F5" => vec![Code::F5], "F6" => vec![Code::F6],
        "F7" => vec![Code::F7], "F8" => vec![Code::F8], "F9" => vec![Code::F9],
        "F10" => vec![Code::F10], "F11" => vec![Code::F11], "F12" => vec![Code::F12],
        "SPACE" => vec![Code::Space], "TAB" => vec![Code::Tab],
        "BACKSPACE" => vec![Code::Backspace], "INSERT" => vec![Code::Insert],
        "DELETE" => vec![Code::Delete], "HOME" => vec![Code::Home], "END" => vec![Code::End],
        "PAGEUP" => vec![Code::PageUp], "PAGEDOWN" => vec![Code::PageDown],
        "UP" => vec![Code::ArrowUp], "DOWN" => vec![Code::ArrowDown],
        "LEFT" => vec![Code::ArrowLeft], "RIGHT" => vec![Code::ArrowRight],
        _ => Vec::new(),
    }
}

/// 热键管理：注册一批快捷键，维护共享的「热键 id -> 动作」映射。
/// 注意：manager 必须在主线程（有消息循环的线程）创建和注册。
pub struct Hotkeys {
    mgr: GlobalHotKeyManager,
    current: Vec<HotKey>,
    actions: SharedActions,
}

impl Hotkeys {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            mgr: GlobalHotKeyManager::new().map_err(|e| anyhow::anyhow!("{e}"))?,
            current: Vec::new(),
            actions: Arc::new(Mutex::new(HashMap::new())),
        })
    }

    /// 给监听线程用的动作映射句柄。
    pub fn actions_handle(&self) -> SharedActions {
        self.actions.clone()
    }

    /// 注销当前所有热键。
    pub fn clear(&mut self) {
        if !self.current.is_empty() {
            let _ = self.mgr.unregister_all(&self.current);
        }
        self.current.clear();
        if let Ok(mut a) = self.actions.lock() {
            a.clear();
        }
    }

    /// 注册一个快捷键并绑定动作。数字键会同时注册主键盘和小键盘两个键位，
    /// 任意一个注册成功就算成功。
    pub fn register(&mut self, combo: &str, action: HkAction) -> bool {
        let hks = parse_all(combo);
        if hks.is_empty() {
            log::warn!("无法解析快捷键「{combo}」");
            return false;
        }
        let mut any_ok = false;
        for hk in hks {
            match self.mgr.register(hk) {
                Ok(()) => {
                    self.current.push(hk);
                    if let Ok(mut a) = self.actions.lock() {
                        a.insert(hk.id(), action.clone());
                    }
                    any_ok = true;
                }
                Err(e) => {
                    log::warn!("注册快捷键「{combo}」失败（可能被其它程序占用）：{e}");
                }
            }
        }
        any_ok
    }
}

/// 在独立线程里阻塞监听热键事件，触发时回调 `on_action`。
/// 不依赖 UI 刷帧，窗口被遮挡/最小化时也能响应。
pub fn spawn_listener<F>(actions: SharedActions, on_action: F)
where
    F: Fn(HkAction) + Send + 'static,
{
    let _ = std::thread::Builder::new()
        .name("hotkeys".into())
        .spawn(move || {
            let rx = GlobalHotKeyEvent::receiver();
            while let Ok(ev) = rx.recv() {
                if ev.state == HotKeyState::Pressed {
                    let act = actions.lock().ok().and_then(|m| m.get(&ev.id).cloned());
                    if let Some(a) = act {
                        on_action(a);
                    }
                }
            }
        });
}
