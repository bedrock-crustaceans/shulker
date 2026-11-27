import init, { Session, shulker_version } from "./pkg/shulker_wasm.js";

let readyPromise = null;

async function ensureReady() {
  if (!readyPromise) {
    readyPromise = (async () => {
      await init();
      const version = shulker_version();
      self.postMessage({ type: "ready", version });
      return version;
    })();
  }
  return readyPromise;
}

ensureReady().catch((err) => {
  self.postMessage({ type: "init_error", message: String(err) });
});

self.addEventListener("message", async (event) => {
  const msg = event.data;
  if (!msg || msg.type !== "validate") return;

  const start = performance.now();
  let session = null;
  try {
    await ensureReady();
    self.postMessage({ type: "status", text: "Loading packs into WASM..." });

    session = new Session();

    for (const archive of msg.archives || []) {
      session.add_archive(archive.name, new Uint8Array(archive.buffer));
    }

    for (const dir of msg.dirs || []) {
      const dirId = session.begin_dir(dir.name);
      for (const file of dir.files) {
        session.add_dir_file(dirId, file.path, new Uint8Array(file.buffer));
      }
    }

    if (msg.vanillaFiles && msg.vanillaFiles.length > 0) {
      self.postMessage({
        type: "status",
        text: `Loading ${msg.vanillaFiles.length} vanilla files...`,
      });
      for (const vf of msg.vanillaFiles) {
        session.add_vanilla_file(vf.path, new Uint8Array(vf.buffer));
      }
    }

    self.postMessage({ type: "status", text: "Checking definitions and links..." });
    const rawJson = session.validate(JSON.stringify(msg.options || {}));
    const data = JSON.parse(rawJson);
    const elapsedMs = Math.round(performance.now() - start);
    self.postMessage({ type: "result", data, elapsedMs });
  } catch (err) {
    self.postMessage({
      type: "error",
      message: err instanceof Error ? `${err.message}` : String(err),
    });
  } finally {
    if (session) {
      try {
        session.free();
      } catch {
        // ignore double-free if wasm panicked
      }
    }
  }
});
