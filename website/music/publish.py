#!/usr/bin/env python3
# [INPUT]: 读取 Suno 导出文件、乔木音乐曲目和现有目录。
# [OUTPUT]: 生成试听、下载、网页和桌面端共用的配乐目录。
# [POS]: 在音频上传完成后原子切换线上目录。
# [PROTOCOL]: Update this header on change, then check AGENTS.md

import argparse
import hashlib
import html
import json
import math
import re
import shutil
import subprocess
import tempfile
import unicodedata
import urllib.parse
import urllib.request
from pathlib import Path
from xml.sax.saxutils import escape as xml_escape

HERE = Path(__file__).resolve().parent
SITE = "https://capwise.qiaomu.ai"
QIAOMU = "https://music.qiaomu.ai"
REMOTE = "/var/www/capwise"
MAX_AUDIO = 32 * 1024 * 1024
MAX_PREVIEW = 512 * 1024
MAX_CATALOG = 2 * 1024 * 1024
CC0 = "CC0 1.0"
SUNO = "乔木配乐授权"
LICENSES = {
    CC0: "可用于个人及商业作品。许可为 CC0 1.0。",
    SUNO: "授权个人及商业视频、播客和直播使用。不得将原始音频作为独立曲目再发行或转售。",
}
SEED = (
    ("exploration-theme", "Exploration Theme", "Cleyton Kauffman", ["氛围", "片头"], "https://opengameart.org/content/exploration-theme", "65b452aeaf9cc3f0d592bc4909cac367577ce0c4043a015276ed48f99d7e84c4"),
    ("beyond-piano-edit", "Beyond (Piano Edit)", "Pablo Perez", ["钢琴", "口播"], "https://freemusicarchive.org/music/pablo-perez/single/beyond-piano-edit/", "6cca5621080f091d4eb53f83ebfaff2683b937ed1eec1f80c0546a9f8ec1f3e2"),
    ("coffee-house-bump", "Coffee House Bump", "OatCog", ["Lo-fi", "轻快"], "https://opengameart.org/content/coffee-house-bump", "d8285fdb48c664b05752dbdc547db7e30e17de128250af6b3e0fccf0bfe3b079"),
    ("electron", "Electron", "JMHBM (Beat Mekanik)", ["电子", "科技"], "https://freemusicarchive.org/music/beat-mekanik/single/electron-free-stems/", "d2723c23900ba6fd5ebbb58995e399182a9f06d6453792f2855aa7c2b7edba27"),
)


class NoRedirect(urllib.request.HTTPRedirectHandler):
    def redirect_request(self, *args, **kwargs):
        return None


def request(url, limit):
    opener = urllib.request.build_opener(NoRedirect())
    with opener.open(url, timeout=30) as reply:
        data = reply.read(limit + 1)
    if not data or len(data) > limit:
        raise ValueError(f"资源为空或超过大小上限：{url}")
    return data


def run(*command, **kwargs):
    return subprocess.run(command, check=True, **kwargs)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def file_digest(path):
    checksum = hashlib.sha256()
    with path.open("rb") as audio:
        for block in iter(lambda: audio.read(1024 * 1024), b""):
            checksum.update(block)
    return checksum.hexdigest()


def format_time(seconds):
    minutes, seconds = divmod(round(seconds), 60)
    return f"{minutes}:{seconds:02}"


def store(media, path, limit):
    if not 0 < path.stat().st_size <= limit:
        raise ValueError(f"音频超过大小上限：{path.name}")
    identity = file_digest(path)
    destination = media / f"{identity}.mp3"
    if not destination.exists():
        shutil.copyfile(path, destination)
    return identity


def track_id(title, source_hash):
    ascii_title = unicodedata.normalize("NFKD", title).encode("ascii", "ignore").decode()
    slug = re.sub("[^a-z0-9]+", "-", ascii_title.lower()).strip("-")[:60].rstrip("-")
    return f"{slug or 'track'}-{source_hash[:12]}"


def encode(source, metadata, media, scratch):
    probe = run(
        "ffprobe", "-v", "error", "-protocol_whitelist", "file,pipe",
        "-select_streams", "a:0", "-show_entries", "stream=codec_name:format=duration",
        "-of", "json", str(source), capture_output=True, timeout=30,
    )
    info = json.loads(probe.stdout)
    streams = info.get("streams", [])
    duration = float(info.get("format", {}).get("duration", 0))
    if not streams or not math.isfinite(duration) or not 0.5 <= duration <= 3600:
        raise ValueError(f"音频时长须在 0.5 秒至 1 小时之间：{source.name}")
    source_hash = file_digest(source)
    if metadata.get("source_sha256", source_hash) != source_hash:
        raise ValueError(f"音源校验失败：{source.name}")
    ffmpeg = ("ffmpeg", "-nostdin", "-y", "-v", "error", "-protocol_whitelist", "file,pipe")
    mapping = ("-map", "0:a:0", "-c:a", "libmp3lame")
    full = source
    if source.suffix.lower() != ".mp3" or streams[0]["codec_name"] != "mp3":
        full = scratch / "track.mp3"
        run(*ffmpeg, "-i", str(source), *mapping, "-ac", "2", "-q:a", "2", str(full), timeout=120)
    preview = scratch / "preview.mp3"
    start = min(duration * 0.20, max(0, duration - 20))
    run(
        *ffmpeg, "-ss", f"{start:.3f}", "-i", str(source), "-t", "20", *mapping,
        "-b:a", "80k", "-map_metadata", "-1", str(preview), timeout=60,
    )
    return {
        "id": metadata.get("id") or track_id(metadata["title"], source_hash),
        "title": metadata["title"],
        "artist": metadata["artist"],
        "tags": metadata["tags"],
        "source": metadata.get("source", ""),
        "license": metadata["license"],
        "duration": round(duration, 3),
        "preview": store(media, preview, MAX_PREVIEW),
        "audio": store(media, full, MAX_AUDIO),
        "sourceHash": source_hash,
    }


def checked_text(value, maximum):
    if not isinstance(value, str) or not value.strip() or len(value) > maximum:
        raise ValueError("曲目名称、作者或标签为空，或超过长度限制")
    return value.strip()


def source_url(value):
    if value:
        parts = urllib.parse.urlsplit(value)
        if parts.scheme != "https" or not parts.hostname or parts.username or parts.password:
            raise ValueError("曲目来源须为公开的 HTTPS 页面")
    return value


def qiaomu_metadata(identity, scratch, tags):
    if not re.fullmatch(r"[0-9a-f]{32}", identity):
        raise ValueError("乔木音乐曲目 ID 应为 32 位小写十六进制")
    item = json.loads(request(f"{QIAOMU}/api/public/tracks/{identity}", MAX_CATALOG))
    item = item.get("track", item)
    if item.get("id") != identity or item.get("contentType") != "audio/mpeg":
        raise ValueError(f"乔木音乐接口返回了不匹配的曲目：{identity}")
    provenance = item.get("source", "").lower()
    if "suno" not in provenance or any(word in provenance for word in ("翻唱", "cover", "remix", "重混")):
        raise ValueError(f"该曲目未标为 Suno 原创，或标记为翻唱或重混：{item['title']}")
    if not re.fullmatch(r"https://music\.qiaomu\.ai/music/[a-zA-Z0-9._-]+\.mp3", item["audioUrl"]):
        raise ValueError("乔木音乐接口返回了未知音频地址")
    data = request(item["audioUrl"], MAX_AUDIO)
    if len(data) != item.get("size"):
        raise ValueError(f"乔木音乐音频长度不匹配：{identity}")
    path = scratch / f"{identity}.mp3"
    path.write_bytes(data)
    return path, dict(id=f"qiaomu-{identity}", title=checked_text(item["title"], 120),
                      artist=checked_text(item["artist"], 100), tags=tags,
                      source=source_url(item["pageUrl"]), license=SUNO)


def input_files(folder, tags):
    metadata_path = folder / "tracks.json"
    metadata = json.loads(metadata_path.read_text()) if metadata_path.is_file() else {}
    if not isinstance(metadata, dict):
        raise ValueError("tracks.json 应为按文件名索引的对象")
    files = sorted(p for p in folder.iterdir() if p.is_file() and p.suffix.lower() in {".mp3", ".wav"})
    if not files:
        raise ValueError("目录内没有 MP3 或 WAV 文件")
    if metadata.keys() - {p.name for p in files}:
        raise ValueError("tracks.json 包含未找到的音频文件")
    for path in files:
        item = metadata.get(path.name, {})
        if not isinstance(item, dict):
            raise ValueError(f"曲目元数据应为对象：{path.name}")
        file_tags = item.get("tags", tags)
        if not isinstance(file_tags, list) or len(file_tags) > 8:
            raise ValueError(f"每首配乐最多八个标签：{path.name}")
        yield path, {
            "title": checked_text(item.get("title", path.stem), 120),
            "artist": checked_text(item.get("artist", "乔木"), 100),
            "tags": [checked_text(tag, 24) for tag in file_tags],
            "source": source_url(item.get("source", "")),
            "license": SUNO,
        }


def catalog(raw):
    if not raw:
        return {}
    if len(raw) > MAX_CATALOG:
        raise ValueError("曲库目录超过大小上限")
    data = json.loads(raw)
    if data["schema"] != 1:
        raise ValueError("曲库目录版本不受支持")
    tracks = {}
    for item in data["tracks"]:
        identity = item["id"]
        if (not re.fullmatch(r"[a-z0-9-]{1,80}", identity) or identity in tracks
                or any(not re.fullmatch(r"[0-9a-f]{64}", item[key]) for key in ("preview", "audio"))
                or item["license"] not in LICENSES):
            raise ValueError("曲库目录包含非法曲目")
        tracks[identity] = item
    return tracks


def page(title, description, path, main, revision, home=False):
    e = html.escape
    return f"""<!doctype html>
<html lang="zh-CN">
<head>
	<meta charset="utf-8">
	<meta name="viewport" content="width=device-width, initial-scale=1">
	<title>{e(title)}</title>
	<meta name="description" content="{e(description)}">
	<link rel="canonical" href="{SITE}{path}">
	<link rel="icon" href="/assets/app-icon.png">
	<link rel="stylesheet" href="/music/library.css?v={revision}">
	<script src="/music/library.js?v={revision}" defer></script>
</head>
<body>
	<header class="site-bar"><div class="frame">
		<a class="brand" href="/"><img src="/assets/app-icon.png" alt="" width="30" height="30">灵录 Capwise</a>
		<nav aria-label="主导航"><a href="/music/"{' aria-current="page"' if home else ''}>配乐库</a><a href="/#download">下载灵录</a></nav>
	</div></header>
	<main class="frame">{main}</main>
	<footer class="frame site-foot"><a href="{QIAOMU}/">乔木音乐网 ↗</a></footer>
</body>
</html>
"""


def track_card(item, linked=True):
    e = html.escape
    title = e(item["title"])
    artist = e(item["artist"])
    duration = format_time(item["duration"])
    url = f'/music/{item["id"]}/'
    name = f'<h2><a href="{url}">{title}</a></h2>' if linked else f"<h1>{title}</h1>"
    tags = "".join(f"<span>{e(tag)}</span>" for tag in item["tags"])
    search = e(" ".join((item["title"], item["artist"], *item["tags"])))
    return f"""<article class="track" data-title="{title}" data-search="{search}">
	<div class="track-info">{name}<p>{artist} <span aria-hidden="true">·</span> {duration}</p>
	<div class="tags">{tags}</div></div>
	<div class="track-controls">
		<button class="preview" type="button" aria-label="试听 {title}" hidden><span aria-hidden="true">▶</span> <span class="preview-label">试听</span></button>
		<audio controls preload="none" src="/music/media/{item["preview"]}.mp3" aria-label="试听 {title}"></audio>
		<a class="download" href="/music/media/{item["audio"]}.mp3" download="{title}.mp3" aria-label="下载 {title}">下载 MP3</a>
	</div>
</article>"""


def build(output, tracks):
    root = output / "music"
    css = (HERE / "library.css").read_bytes()
    js = (HERE / "library.js").read_bytes()
    revision = digest(css + js)[:12]
    (root / "library.css").write_bytes(css)
    (root / "library.js").write_bytes(js)
    raw = json.dumps({"schema": 1, "tracks": list(tracks.values())}, ensure_ascii=False, indent=2).encode() + b"\n"
    catalog(raw)
    (root / "catalog.json").write_bytes(raw)
    paths = ["/music/"]
    for track in tracks.values():
        identity = track["id"]
        directory = root / identity
        directory.mkdir()
        path = f"/music/{identity}/"
        paths.append(path)
        license_text = LICENSES[track["license"]]
        source = track["source"]
        source_link = (f'<p><a href="{html.escape(source_url(source))}">查看曲目来源 ↗</a></p>'
                       if source else "")
        if track["license"] == CC0:
            source_link += '<p><a href="https://creativecommons.org/publicdomain/zero/1.0/">CC0 1.0 许可 ↗</a></p>'
        body = (f'<a class="back" href="/music/">← 返回配乐库</a>{track_card(track, False)}'
                f'<section class="license"><h2>使用授权</h2><p>{license_text}</p>{source_link}</section>')
        (directory / "index.html").write_text(page(
            f'{track["title"]} · 灵录配乐库',
            f'试听并下载 {track["artist"]} 的《{track["title"]}》。{license_text}',
            path, body, revision,
        ))
    cards = "".join(track_card(item) for item in tracks.values())
    count = len(tracks)
    body = f"""<section class="intro">
	<h1>配乐库</h1><p>为录屏、教程和视频找到合适的背景音乐。</p>
</section>
<section class="collection" aria-label="曲目">
	<div class="tools" hidden>
		<label for="music-search">搜索配乐</label>
		<input type="search" id="music-search" placeholder="歌名、作者、风格或用途" autocomplete="off">
		<p class="result" id="music-count" role="status">{count} 首配乐</p>
	</div>
	<div class="tracks">{cards}</div>
	<p id="music-empty" hidden>没有找到符合条件的配乐。</p>
</section>"""
    (root / "index.html").write_text(page(
        "配乐库 · 灵录 Capwise", "搜索、试听并下载灵录配乐库里的背景音乐。",
        "/music/", body, revision, home=True,
    ))
    sitemap = "".join(f"<url><loc>{xml_escape(SITE + path)}</loc></url>" for path in paths)
    (root / "sitemap.xml").write_text(
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        f'<urlset xmlns="http://www.sitemaps.org/schemas/sitemap/0.9">{sitemap}</urlset>\n'
    )


def publish(host, output, old):
    media = output / "music" / "media"
    names = sorted(path.name for path in media.glob("*.mp3"))
    root = output / "music"
    release = digest(b"".join(
        file.relative_to(output).as_posix().encode() + b"\0" + file.read_bytes()
        for file in sorted(root.rglob("*")) if file.is_file() and file.parent != media
    ))[:20]
    new = catalog((root / "catalog.json").read_bytes())
    required = sorted({item[key] for item in new.values() for key in ("audio", "preview")})
    (output / "required.sha256").write_text("".join(f"{identity}\n" for identity in required))
    stage = run("ssh", host, f"mktemp -d {REMOTE}/music-staging.XXXXXXXX", capture_output=True, text=True).stdout.strip()
    if not re.fullmatch(rf"{REMOTE}/music-staging\.[A-Za-z0-9]+", stage):
        raise ValueError("服务器返回非法的暂存路径")
    try:
        run("rsync", "-az", f"{output}/music/", f"{host}:{stage}/music/")
        run("rsync", "-az", str(output / "required.sha256"), f"{host}:{stage}/")
        remote = r"""
set -euo pipefail
root=$1 stage=$2 release=$3 old=$4
exec 9>"$root/music.lock"
flock -x 9
live="$root/music-current/music/catalog.json"
current=empty
if test -f "$live"; then current=$(sha256sum "$live" | cut -d' ' -f1); fi
if test "$current" != "$old"; then echo '曲库刚被更新，请重新运行。' >&2; exit 1; fi
install -d "$root/music-media" "$root/music-releases"
for path in "$stage"/music/media/*.mp3; do
    test -f "$path" || continue
    name=${path##*/}
    printf '%s  %s\n' "${name%.mp3}" "$path" | sha256sum --check --status
    dest="$root/music-media/$name"
    if test -f "$dest"; then
        printf '%s  %s\n' "${name%.mp3}" "$dest" | sha256sum --check --status
    else
        chmod 644 "$path"
        mv -- "$path" "$dest"
    fi
done
while IFS= read -r hash; do test -s "$root/music-media/$hash.mp3"; done < "$stage/required.sha256"
rm -- "$stage/required.sha256"
if test -d "$stage/music/media"; then rm -r -- "$stage/music/media"; fi
chmod 755 "$stage"
if ! test -d "$root/music-releases/$release"; then mv -- "$stage" "$root/music-releases/$release"; fi
ln -sfn "$root/music-releases/$release" "$root/music-current.next"
mv -T "$root/music-current.next" "$root/music-current"
"""
        remote_run = "bash -s -- " + " ".join((REMOTE, stage, release, digest(old) if old else "empty"))
        run("ssh", host, remote_run, input=remote, text=True)
    finally:
        run("ssh", host, f"test ! -d {stage} || rm -r -- {stage}")
    print(f"已发布 {len(new)} 首配乐（{len(names)} 个音频文件） https://capwise.qiaomu.ai/music/")


def main():
    parser = argparse.ArgumentParser(description="生成和发布灵录配乐库")
    parser.add_argument("folder", nargs="?", type=Path, help="Suno 导出的 MP3 或 WAV 文件目录")
    parser.add_argument("--seed", action="store_true", help="导入灵录内置的四首 CC0 配乐")
    parser.add_argument("--suno-pro", action="store_true", help="确认对该批原创曲目有配乐分发授权")
    parser.add_argument("--from-qiaomu", action="append", default=[], metavar="ID", help="从乔木音乐导入指定曲目 ID，可多次使用")
    parser.add_argument("--list-qiaomu", action="store_true", help="列出乔木音乐的公开曲目和 ID")
    parser.add_argument("--tag", action="append", default=[], help="批次标签，可多次使用")
    parser.add_argument("--remove", action="append", default=[], metavar="ID", help="从目录下架指定曲目")
    parser.add_argument("--base", type=Path, help="本地预览使用的现有 catalog.json")
    parser.add_argument("--output", type=Path, help="把生成结果保存到一个新目录")
    parser.add_argument("--publish", action="store_true", help="上传并发布到灵录官网")
    parser.add_argument("--host", default="qiaomu-wx", help="官网的 SSH 别名")
    args = parser.parse_args()
    if args.list_qiaomu:
        for track in json.loads(request(f"{QIAOMU}/api/public/tracks", MAX_CATALOG))["tracks"]:
            print(f'{track["id"]}  {track["title"]}  {track.get("source", "")}')
        return
    if (args.folder or args.from_qiaomu) and not args.suno_pro:
        parser.error("确认授权后使用 --suno-pro")
    if args.publish and args.base:
        parser.error("线上发布以服务器上的当前目录为基准")
    if not args.publish and not args.output:
        parser.error("指定 --output 查看生成结果，或指定 --publish 上线")
    if args.output and args.output.exists():
        parser.error("--output 必须是新目录")
    if not re.fullmatch(r"[a-zA-Z0-9][a-zA-Z0-9.-]*", args.host):
        parser.error("SSH 主机别名无效")
    if len(args.tag) > 8:
        parser.error("每首配乐最多八个标签")
    tags = [checked_text(tag, 24) for tag in args.tag]
    if args.publish:
        old = run("ssh", args.host, f"live={REMOTE}/music-current/music/catalog.json; if test -f \"$live\"; then cat \"$live\"; fi",
                  capture_output=True).stdout
    else:
        old = args.base.read_bytes() if args.base else b""
    tracks = catalog(old)
    media_sources = {item.get("sourceHash") for item in tracks.values()}
    with tempfile.TemporaryDirectory(prefix="capwise-music-") as tmp:
        scratch = Path(tmp)
        output = scratch / "site"
        media = output / "music" / "media"
        media.mkdir(parents=True)
        inputs = []
        if args.seed:
            assets = HERE.parent.parent / "apps/desktop-gpui/assets/music"
            for identity, title, artist, track_tags, source, sha in SEED:
                inputs.append((assets / f"{identity}.mp3", dict(
                    id=identity, title=title, artist=artist, tags=track_tags, source=source,
                    license=CC0, source_sha256=sha,
                )))
        if args.folder:
            inputs.extend(input_files(args.folder, tags))
        for identity in dict.fromkeys(args.from_qiaomu):
            inputs.append(qiaomu_metadata(identity, scratch, tags))
        for path, meta in inputs:
            if not 0 < path.stat().st_size <= 512 * 1024 * 1024:
                raise ValueError(f"原始音频为空或超过 512 MB：{path.name}")
            item = encode(path.resolve(), meta, media, scratch)
            if item["id"] not in tracks and item["sourceHash"] in media_sources:
                print(f"跳过重复音频：{item['title']}")
                continue
            tracks[item["id"]] = item
            media_sources.add(item["sourceHash"])
            print(f"{item['id']}  {item['title']}  {format_time(item['duration'])}")
        for identity in args.remove:
            if tracks.pop(identity, None) is None:
                raise ValueError(f"没有找到要下架的曲目：{identity}")
        referenced = {f"{item[key]}.mp3" for item in tracks.values() for key in ("audio", "preview")}
        if args.base:
            for name in referenced:
                if (media / name).exists():
                    continue
                previous = args.base.parent / "media" / name
                if not previous.is_file() or file_digest(previous) != previous.stem:
                    raise ValueError(f"本地基准目录缺少有效音频：{name}")
                shutil.copyfile(previous, media / name)
        for asset in media.iterdir():
            if asset.name not in referenced:
                asset.unlink()
        build(output, tracks)
        if args.output:
            shutil.copytree(output, args.output)
            print(f"已生成 {len(tracks)} 首配乐到 {args.output.resolve()}")
        if args.publish:
            publish(args.host, output, old)


if __name__ == "__main__":
    try:
        main()
    except (OSError, ValueError, KeyError, TypeError, subprocess.SubprocessError) as error:
        raise SystemExit(f"配乐发布失败：{error}") from None
