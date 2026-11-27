# 桌面编辑器媒体与应用更新

> L2 | Parent: ../../../AGENTS.md

`editor_audio.rs` 复用项目素材保存流程，提供内置和在线配乐入口。
`editor_window.rs` 提供工程时间线和配音侧栏的预览通道。
`app_windows.rs` 按播放请求标记引擎停止事件。
`editor_voiceover.rs` 确定配音目标，写入可撤销的时间线修改。
`editor_voiceover/` 同步录制与回看，在画面旁显示配音控件。
`editor_panels.rs` 在音频属性中提供已有配音的重录入口。
`voiceover_recording.rs` 用预览时钟控制麦克风采样，保留可恢复素材。
`online_music.rs` 读取官网目录、筛选曲目并验证下载前后的文件身份。
`updates.rs` 确认任务空闲、保存工程，再请求安装重启。
`macos_update.rs` 等待旧进程结束，持单实例锁执行安装。
`installer.rs` 验签下载，重建增量包并替换应用。
`single_instance.rs` 在启动与安装之间共享单实例锁。
`main.rs` 在启动桌面界面前分流安装进程。
`menus.rs` 保存待写入的编辑，再请求应用退出。
`platform.rs` 接入原生退出回调，支持撤回退出授权。
`settings_window.rs` 显示检查更新和重启入口。

[PROTOCOL]: Update this header on change, then check AGENTS.md
