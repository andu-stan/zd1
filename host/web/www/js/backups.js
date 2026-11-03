// The MCU's backups: host/core's tar, downloaded, and a copy in this browser (IndexedDB).
import { installMcu } from "./software.js";
import { ask, download, el } from "./ui.js";

// ---- the backups: host/core's tar, downloaded, and a copy in this browser (IndexedDB) ------------------------------

/// The browser's database, or an error when it has no storage for this page (a private window, storage blocked: the
/// open may never answer, hence the 2 s).
export function idb() {
  return new Promise((res, rej) => {
    setTimeout(() => rej(new Error("this browser gives the page no storage")), 2000);
    const r = indexedDB.open("zeppelin-air", 2);
    r.onupgradeneeded = () => {
      if (!r.result.objectStoreNames.contains("mcu-backups"))
        r.result.createObjectStore("mcu-backups", { keyPath: "id", autoIncrement: true });
    };
    r.onsuccess = () => res(r.result);
    r.onerror = () => rej(r.error);
  });
}
export async function store(mode, f) {
  const db = await idb();
  return new Promise((res, rej) => {
    const t = db.transaction("mcu-backups", mode);
    const r = f(t.objectStore("mcu-backups"));
    t.oncomplete = () => res(r?.result);
    t.onerror = () => rej(t.error);
  });
}
export const backups = async () => ((await store("readonly", (s) => s.getAll())) ?? []).sort((a, b) => b.id - a.id);

/// The MCU's backup before a flash (a tar: the app region, the user page, its manifest): a download, and a copy
/// here. The flash waits for this; without a copy here the person decides whether it goes on (the download alone).
export async function keepBackup(tar, name, holds) {
  download(name, new Blob([tar], { type: "application/x-tar" }));
  try {
    await store("readwrite", (s) => s.add({ date: new Date().toISOString(), holds, name, tar }));
  } catch (e) {
    const go = await ask(
      "The backup is only a download",
      `This browser didn't keep a copy (${e.message}). The download ${name} is the way back: check that it is ` +
        "saved before going on.",
      "Go on",
    );
    if (!go) throw new Error("stopped: the backup is only in the download");
  }
}

export async function drawBackups(into, onRestore) {
  let list = [];
  try {
    list = await backups();
  } catch (e) {
    into.replaceChildren(
      el("p", {
        className: "small",
        textContent: `None kept here: ${e.message}. The downloads (.tar) install as MCU firmware under Advanced.`,
      }),
    );
    return;
  }
  into.replaceChildren(
    ...(list.length
      ? list.map((b) => {
          const r = el(
            "div",
            { className: "row item" },
            el("span", {
              textContent: `${new Date(b.date).toLocaleString()}: ${b.holds === "unknown" ? "the firmware before an install" : b.holds}`,
            }),
            el("span", { className: "grow" }),
          );
          const go = el("button", { textContent: "Restore" });
          go.onclick = () => onRestore(b);
          const dl = el("button", { className: "quiet", textContent: "Download" });
          dl.onclick = () => download(b.name, new Blob([b.tar], { type: "application/x-tar" }));
          const rm = el("button", { className: "quiet", textContent: "Delete" });
          rm.onclick = async () => {
            await store("readwrite", (s) => s.delete(b.id));
            drawBackups(into, onRestore);
          };
          r.append(go, dl, rm);
          return r;
        })
      : [
          el("p", {
            className: "small",
            textContent: "None: this browser keeps one each time it installs MCU firmware.",
          }),
        ]),
  );
}

export const restoreBackup = (b) => installMcu(b.name, b.tar);
