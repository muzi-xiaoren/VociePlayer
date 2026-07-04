# VociePlayer

轻量级 Windows 声卡音效热键工具（类 Soundpad）。按下全局快捷键，把音效混进麦克风，
一起打游戏 / 开黑的人就能听到你的音效——同时**不影响你正常说话**。

- 纯 Rust + egui，单个 exe，体积小、内存占用低、启动快
- 多配置（每个配置一个文件夹），把音频拖进去自动识别
- 每个音效绑全局快捷键，另有「停止所有音效」快捷键
- 可选：开机自启、耳机监听（自己也能听到音效）

## 工作原理

游戏 / Discord 是从「麦克风」采集声音的。VociePlayer 自己做混音：

```
真麦克风 ──┐
          ├─→ VociePlayer 混音 ──→ CABLE Input ──→ 游戏麦克风 = CABLE Output
音效文件 ──┘                        (VB-CABLE 虚拟声卡)
```

因为混音在程序内部完成，所以**不需要 VoiceMeeter**，只需要一根「哑管道」虚拟声卡，
用免费的 [VB-CABLE](https://vb-audio.com/Cable/) 即可。

## 使用步骤

1. 安装 [VB-CABLE](https://vb-audio.com/Cable/) 并重启电脑。
2. 打开 VociePlayer，**输出设备**选 `CABLE Input (VB-Audio Virtual Cable)`。
3. 在游戏 / Discord 里，把**麦克风**设成 `CABLE Output (VB-Audio Virtual Cable)`。
4. 点「打开文件夹」，把 mp3 / wav / ogg / flac 拖进去，会自动出现在列表里。
5. 给每个音效点「设快捷键」，按下想要的组合键即可。
6.（可选）想自己也能听到音效，把「监听设备」设成你的耳机。

> 注意：游戏麦克风设成了 CABLE Output，所以 **VociePlayer 需要一直开着**转发真麦；
> 关掉程序后游戏里会没声音。可勾选「开机自启动」。

## 数据目录

配置和音频放在 `%APPDATA%\VociePlayer\`：

```
%APPDATA%\VociePlayer\
├─ config.json
└─ profiles\
   └─ 默认\
      ├─ xxx.mp3
      └─ _bindings.json   # 快捷键 / 音量绑定
```

## 从源码构建

需要 Rust 工具链（`rustup`）。仅支持 Windows 运行（音频用 WASAPI）。

```bash
cargo run            # 调试运行
cargo build --release
```

发布：推一个 `v` 开头的 tag（如 `v0.0.1`），GitHub Actions 会自动构建
绿色版 zip + 安装包并发到 Release。

## 许可

MIT
