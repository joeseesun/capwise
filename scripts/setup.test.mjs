import assert from "node:assert/strict";
import { execFile as execFileCallback } from "node:child_process";
import { createHash } from "node:crypto";
import { once } from "node:events";
import {
	copyFileSync,
	existsSync,
	mkdirSync,
	mkdtempSync,
	readFileSync,
	rmSync,
	writeFileSync,
} from "node:fs";
import { createServer } from "node:http";
import { tmpdir } from "node:os";
import path from "node:path";
import test from "node:test";
import { fileURLToPath } from "node:url";
import { promisify } from "node:util";

const execFile = promisify(execFileCallback);
const repositoryRoot = fileURLToPath(new URL("../", import.meta.url));

test(
	"setup retries failed downloads without poisoning the archive cache",
	{
		skip: process.platform !== "linux" && "Linux setup fixture",
		timeout: 60_000,
	},
	async (t) => {
		const fixture = mkdtempSync(path.join(tmpdir(), "cap-setup-"));
		t.after(() => rmSync(fixture, { recursive: true, force: true }));
		for (const file of [
			"package.json",
			"scripts/setup.js",
			"scripts/linux-bundle-config.mjs",
			"apps/desktop-gpui/tauri.conf.json",
		]) {
			const destination = path.join(fixture, file);
			mkdirSync(path.dirname(destination), { recursive: true });
			copyFileSync(path.join(repositoryRoot, file), destination);
		}

		const nativeName = "native-deps-x86_64-linux-gnu.tar.xz";
		const onnxName = "onnxruntime-linux-x64-1.24.3.tgz";
		const nugetName = "microsoft.ml.onnxruntime.1.24.3.nupkg";
		const nativeUrl = `/spacedriveapp/native-deps/releases/download/v0.26/${nativeName}`;
		const onnxUrl = `/microsoft/onnxruntime/releases/download/v1.24.3/${onnxName}`;
		const nugetUrl = `/v3-flatcontainer/microsoft.ml.onnxruntime/1.24.3/${nugetName}`;
		const nativeLibrary = "synthetic native library\n";
		const onnxLibrary = "synthetic ONNX library\n";
		const archives = new Map();
		for (const [url, entry, contents] of [
			[nativeUrl, "lib/libavcodec.so.61", nativeLibrary],
			[onnxUrl, `${onnxName.slice(0, -4)}/lib/libonnxruntime.so`, onnxLibrary],
		]) {
			const archive = path.join(fixture, "assets", path.basename(url));
			const directory = `${archive}.contents`;
			mkdirSync(path.dirname(path.join(directory, entry)), { recursive: true });
			writeFileSync(path.join(directory, entry), contents);
			if (url === nativeUrl) {
				for (const library of [
					"avcodec",
					"avdevice",
					"avfilter",
					"avformat",
					"avutil",
					"swresample",
					"swscale",
				]) {
					writeFileSync(
						path.join(directory, "lib", `lib${library}.so`),
						contents,
					);
				}
			}
			await execFile(
				"tar",
				[url.endsWith(".xz") ? "-cJf" : "-czf", archive, "-C", directory, "."],
				{ signal: t.signal },
			);
			archives.set(url, readFileSync(archive));
		}

		let failNative = true;
		let networkFailures = 0;
		const requests = [];
		const server = createServer((request, response) => {
			requests.push(request.url);
			if (failNative && request.url === nativeUrl) {
				response.writeHead(503).end("Service Unavailable\n");
				return;
			}
			if (request.url === nativeUrl && networkFailures > 0) {
				networkFailures--;
				request.socket.destroy();
				return;
			}
			const archive = archives.get(request.url);
			response.writeHead(archive ? 200 : 404);
			response.end(archive ?? "Unexpected setup download");
		});
		server.listen(0, "127.0.0.1");
		await once(server, "listening");
		t.after(async () => {
			server.closeAllConnections();
			await promisify(server.close.bind(server))();
		});
		const origin = `http://127.0.0.1:${server.address().port}`;
		const preload = path.join(fixture, "http-fixture.mjs");
		writeFileSync(
			preload,
			`const networkFetch = globalThis.fetch;
globalThis.fetch = (input, options) => {
	const url = new URL(input);
	if (!["https://github.com", "https://api.nuget.org"].includes(url.origin))
		throw new Error(\`Unexpected external request: \${url}\`);
	return networkFetch(new URL(url.pathname, ${JSON.stringify(origin)}), options);
};
`,
		);
		const runSetup = () =>
			execFile(process.execPath, ["--import", preload, "scripts/setup.js"], {
				cwd: fixture,
				signal: t.signal,
				timeout: 20_000,
				env: {
					...process.env,
					NODE_OPTIONS: "",
					RUST_TARGET_TRIPLE: "x86_64-unknown-linux-gnu",
					CAP_USE_SCCACHE: "0",
					TMPDIR: fixture,
				},
			}).then(
				({ stdout, stderr }) => ({ code: 0, stdout, stderr }),
				(error) => {
					assert.equal(typeof error.code, "number", error.message);
					return error;
				},
			);

		const cache = path.join(fixture, "target", `v0.26-${nativeName}`);
		const failed = await runSetup();
		assert.equal(failed.code, 1, failed.stderr);
		assert.deepEqual(requests, [nativeUrl]);
		assert.equal(
			existsSync(cache),
			false,
			"failed downloads must not be cached",
		);
		assert.match(failed.stderr, /HTTP 503/);

		failNative = false;
		networkFailures = 3;
		requests.length = 0;
		const disconnected = await runSetup();
		assert.equal(disconnected.code, 1, disconnected.stderr);
		assert.deepEqual(requests, [nativeUrl, nativeUrl, nativeUrl]);
		assert.equal(existsSync(cache), false);
		assert.match(disconnected.stderr, /UND_ERR_SOCKET/);

		networkFailures = 2;
		for (const expectedRequests of [
			[nativeUrl, nativeUrl, nativeUrl, onnxUrl],
			[],
		]) {
			requests.length = 0;
			const result = await runSetup();
			assert.equal(result.code, 0, result.stderr);
			assert.deepEqual(requests, expectedRequests);
			assert.deepEqual(readFileSync(cache), archives.get(nativeUrl));
			for (const [name, contents] of [
				["libavcodec.so.61", nativeLibrary],
				["libonnxruntime.so", onnxLibrary],
				["libonnxruntime.so.1", onnxLibrary],
			]) {
				for (const directory of [
					"target/debug",
					"target/release",
					"target/x86_64-unknown-linux-gnu/debug",
					"target/x86_64-unknown-linux-gnu/release",
				]) {
					assert.equal(
						readFileSync(path.join(fixture, directory, name), "utf8"),
						contents,
					);
				}
				const gpui = path.join(fixture, "apps/desktop-gpui");
				const config = JSON.parse(
					readFileSync(path.join(gpui, "tauri.linux.conf.json"), "utf8"),
				);
				const source = config.bundle.linux.deb.files[`/usr/lib/cap/${name}`];
				assert.equal(
					readFileSync(path.resolve(gpui, source), "utf8"),
					contents,
				);
			}
			const cargo = readFileSync(
				path.join(fixture, ".cargo/config.toml"),
				"utf8",
			);
			assert.match(cargo, /\[target\.x86_64-unknown-linux-gnu\]/);
			assert.match(
				cargo,
				/ORT_DYLIB_PATH = .*native-deps\/onnxruntime\/lib\/libonnxruntime\.so/,
			);
		}

		const nugetEntry = "runtimes/linux-x64/native/libonnxruntime.so";
		mkdirSync(path.join(fixture, path.dirname(nugetEntry)), {
			recursive: true,
		});
		writeFileSync(path.join(fixture, nugetEntry), onnxLibrary);
		const nugetPath = path.join(fixture, nugetName);
		await execFile("zip", ["-q", nugetPath, nugetEntry], {
			cwd: fixture,
			signal: t.signal,
		});
		const nugetArchive = readFileSync(nugetPath);
		const nugetHash = createHash("sha512")
			.update(nugetArchive)
			.digest("base64");
		const setupPath = path.join(fixture, "scripts/setup.js");
		writeFileSync(
			setupPath,
			readFileSync(setupPath, "utf8").replace(
				/("1\.24\.3":\s*)"[^"]+"/,
				`$1${JSON.stringify(nugetHash)}`,
			),
		);
		rmSync(path.join(fixture, "target", onnxName));
		rmSync(path.join(fixture, "target/native-deps/onnxruntime"), {
			recursive: true,
		});
		archives.delete(onnxUrl);
		archives.set(nugetUrl, Buffer.from("invalid archive"));
		requests.length = 0;
		const invalidFallback = await runSetup();
		assert.equal(invalidFallback.code, 1);
		assert.match(invalidFallback.stderr, /sha512 mismatch/);
		assert.deepEqual(requests, [onnxUrl, nugetUrl]);
		assert.equal(existsSync(path.join(fixture, "target", nugetName)), false);

		archives.set(nugetUrl, nugetArchive);
		requests.length = 0;
		const verifiedFallback = await runSetup();
		assert.equal(verifiedFallback.code, 0, verifiedFallback.stderr);
		assert.deepEqual(requests, [onnxUrl, nugetUrl]);
		assert.equal(
			readFileSync(
				path.join(
					fixture,
					"target/native-deps/onnxruntime/lib/libonnxruntime.so",
				),
				"utf8",
			),
			onnxLibrary,
		);
	},
);
