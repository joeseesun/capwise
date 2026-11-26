# 灵录 Capwise 官网

静态官网，源文件在本目录。生产域名 `capwise.qiaomu.ai` 由现有 VPS 的 Nginx 提供静态文件和 HTTPS。

## 构建与部署

```bash
cd website
npm run build
```

当前生产 VPS 使用 `qiaomu-wx` SSH 别名，发布目录为 `/var/www/capwise/releases/`，Nginx 通过 `/var/www/capwise/current` 提供当前版本。备用站点模板见 `deploy/nginx-capwise.conf`，`scripts/deploy-vps.sh` 默认发布到备用主机 `qiaomu-la`。

`site.config.json` 只记录公开的 Umami 站点 ID。构建检查本地资源存在性、结构化数据和统计 ID，并生成缓存版本及 JSON-LD 的 CSP hash。`dist/` 为生成目录。

## 图片与演示工程

- 工作台截图来自真实安装版与公开演示工程；不发布私人录屏或账户配置。
- NOVA 是原创虚构演示内容，不代表真实第三方产品。`scripts/demo/create-demo.py` 可重新生成可编辑工程，需要 Pillow、FFmpeg、Noto Sans CJK SC 字体以及作为输入的原生 `.cap` 模板与自有音乐。
- 演示视频由应用自带 `cap-cli export` 导出，1080p、30 fps、24 秒。原生文字、字幕、缩放与配乐继续保存在 `.cap` 工程中。
- 截图只进行局部裁切、等比缩放和 WebP 压缩，没有重绘应用控件。
- 图标来自项目已有资源与 Lucide，许可见 `LUCIDE-LICENSE`。打赏和公众号二维码来自维护者提供的个人资料资源。

## 发布检查

检查桌面与手机排版、全部图库标签的键盘切换、放大图、打赏和关注弹窗、复制安装提示词、FAQ，以及浏览器控制台、PWA 和离线页。统计接入需从真实生产浏览器触发唯一事件，并在 Umami 对应站点中确认接收，不能只检查脚本加载。

Apple Silicon 正式版始终使用官方 GitHub 的 `releases/latest/download/Capwise.dmg`。其他平台按已验证的预览版本启用：在 `site.config.json` 的 `previewDownloads` 中填入预览 Release 的 `tag` 和已通过检查的 `platforms`（`windows-x64`、`linux-x64`、`mac-intel`）。部署前回读每个资产的校验和并确认 Release 已公开。更新图片、演示文件时保留对应的验证记录。

## 配乐库

官网的 `/music/` 与灵录的「添加音乐 → 在线配乐」使用同一份 `/music/catalog.json`。播放、搜索和下载公开。项目里新增的配乐会与工程一起保存。曲目页列出来源及适用的使用授权。

首次部署把 `deploy/nginx-capwise-music.conf` 引入生产 Nginx 的 HTTPS server。曲库路由使用独立的 `music-current`，不随官网首页发布覆盖。官网当前生产 SSH 别名为 `qiaomu-wx`。

### 批量上架

在仓库根目录运行，依赖 Python 3、FFmpeg、SSH 与 rsync。有官网 SSH 权限的维护者可直接使用 `--publish`。

```bash
# 先在本地生成试听、网页和目录
python3 website/music/publish.py ~/Music/Suno导出 --suno-pro --tag 口播 --output /tmp/capwise-music-preview
# 上架该目录内所有 MP3 和 WAV
python3 website/music/publish.py ~/Music/Suno导出 --suno-pro --tag 口播 --publish
# 列出乔木音乐已有的歌曲和曲目 ID
python3 website/music/publish.py --list-qiaomu
# 上架乔木音乐里明确选中的原创曲目
python3 website/music/publish.py --from-qiaomu <曲目ID> --suno-pro --tag 片头 --publish
# 从曲库去掉一首曲目，保留旧工程使用的音频
python3 website/music/publish.py --remove <曲目ID> --publish
```

音频文件名默认用作歌名。要设置歌名、作者、标签或来源，可在音频目录加入 `tracks.json`，例如：

```json
{
	"Morning Walk.wav": {
		"title": "晨间散步",
		"artist": "乔木",
		"tags": ["轻快", "口播"],
		"source": "https://music.qiaomu.ai/track/example"
	}
}
```

`--suno-pro` 表示操作者确认拥有本批曲目的分发和下游配乐授权。乔木音乐导入需逐首指定，工具拒绝标记为翻唱或重混的来源。发布器先生成约 20 秒试听和完整 MP3，再上传按 SHA-256 寻址的音频。所有文件到位后，原子切换目录和曲目页。官网和已具备在线配乐功能的客户端会读取新目录。
