//! 平台相关的小工具：开机自启、打开文件夹 / 网页。
//! 非 Windows 平台提供空实现，保证跨平台可编译（本机 Mac 上开发时能过编译）。

/// VB-CABLE 官方下载页。
pub const VBCABLE_URL: &str = "https://vb-audio.com/Cable/";

/// 设置 / 取消开机自启。
#[cfg(windows)]
pub fn set_autostart(enable: bool) -> anyhow::Result<()> {
    use winreg::enums::HKEY_CURRENT_USER;
    use winreg::RegKey;

    let hkcu = RegKey::predef(HKEY_CURRENT_USER);
    let (run, _) = hkcu.create_subkey(r"Software\Microsoft\Windows\CurrentVersion\Run")?;
    const NAME: &str = "VociePlayer";
    if enable {
        let exe = std::env::current_exe()?;
        run.set_value(NAME, &format!("\"{}\"", exe.display()))?;
    } else {
        // 不存在时删除会报错，忽略即可。
        let _ = run.delete_value(NAME);
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn set_autostart(_enable: bool) -> anyhow::Result<()> {
    Ok(())
}

/// 在系统文件管理器里打开一个文件夹。
pub fn open_folder(path: &std::path::Path) {
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer").arg(path).spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(path).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(path).spawn();
    }
}

/// 用默认浏览器打开一个网址。
pub fn open_url(url: &str) {
    #[cfg(windows)]
    {
        // 用 cmd 的 start；空标题参数 "" 不能省。
        let _ = std::process::Command::new("cmd")
            .args(["/C", "start", "", url])
            .spawn();
    }
    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open").arg(url).spawn();
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let _ = std::process::Command::new("xdg-open").arg(url).spawn();
    }
}
