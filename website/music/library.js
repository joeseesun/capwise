/*
 * [INPUT]: 读取预渲染曲目和用户的搜索词。
 * [OUTPUT]: 筛选曲目，切换试听时暂停正在播放的音频。
 * [POS]: 配乐库和独立曲目页的浏览器交互。
 * [PROTOCOL]: Update this header on change, then check AGENTS.md
 */
let playing;
const search = document.querySelector("#music-search");
if (search instanceof HTMLInputElement) {
	const list = document.querySelector(".tracks");
	const rows = Array.from(list?.querySelectorAll("article") ?? []);
	const count = document.querySelector("#music-count");
	const empty = document.querySelector("#music-empty");
	search.closest(".tools")?.removeAttribute("hidden");
	search.addEventListener("input", () => {
		const query = search.value.toLowerCase().trim();
		const terms = query.split(/\s+/);
		const matching = [];
		for (const row of rows) {
			const words = row.getAttribute("data-search")?.toLowerCase() ?? "";
			const matches = terms.every((term) => words.includes(term));
			row.toggleAttribute("hidden", !matches);
			if (matches) {
				matching.push(row);
			} else {
				row.querySelector("audio")?.pause();
			}
		}
		if (query) {
			const rank = (row) => {
				const title = row.getAttribute("data-title")?.toLowerCase() ?? "";
				return title === query ? 0 : title.startsWith(query) ? 1 : 2;
			};
			matching.sort((left, right) => rank(left) - rank(right));
		}
		const ordered = query
			? [...matching, ...rows.filter((row) => row.hasAttribute("hidden"))]
			: rows;
		const active = playing?.paused === false ? playing.closest(".track") : null;
		let next = null;
		for (const row of Array.from(ordered).reverse()) {
			if (row !== active && row.nextElementSibling !== next) {
				list?.insertBefore(row, next);
			}
			next = row;
		}
		if (count) count.textContent = `${matching.length} 首配乐`;
		empty?.toggleAttribute("hidden", matching.length !== 0);
	});
}

for (const audio of document.querySelectorAll(".track audio")) {
	const row = audio.closest(".track");
	const button = row?.querySelector(".preview");
	const label = button?.querySelector(".preview-label");
	const icon = button?.firstElementChild;
	const title = row?.getAttribute("data-title") ?? "";
	if (!(audio instanceof HTMLAudioElement) || !button || !label || !icon)
		continue;
	button.removeAttribute("hidden");
	audio.hidden = true;

	function show(state) {
		label.textContent = state;
		icon.textContent = state === "暂停" ? "Ⅱ" : "▶";
		button.setAttribute("aria-label", `${state} ${title}`);
		row?.classList.toggle("playing", state === "暂停");
	}
	audio.addEventListener("playing", () => show("暂停"));
	audio.addEventListener("pause", () => {
		if (!audio.error) show("试听");
	});
	audio.addEventListener("ended", () => show("试听"));
	button.addEventListener("click", async () => {
		if (!audio.paused) {
			audio.pause();
			return;
		}
		if (playing !== audio) playing?.pause();
		playing = audio;
		if (audio.error) audio.load();
		show("载入中");
		try {
			await audio.play();
		} catch (error) {
			if (error instanceof DOMException && error.name === "AbortError") {
				show("试听");
				return;
			}
			show("重试");
		}
	});
}
