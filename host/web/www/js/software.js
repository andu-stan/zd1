// The software: the bootloader's view (install, repair, roll back), the slots, the releases, installs from files.
import { imageInfo } from "../pkg/zd1_web.js";
import { backups, drawBackups, restoreBackup } from "./backups.js";
import { act, connect, events, job, show } from "./connection.js";
import { app } from "./shared.js";
import { refresh } from "./speaker.js";
import { $, ask, el, failed, names, notice } from "./ui.js";

// ---- the bootloader: the installer ------------------------------------------------------------------------------

export async function drawDfu() {
  let st;
  try {
    st = await app.bl.state();
  } catch (e) {
    return failed(e);
  }
  const last = st.boot.at(-1);
  const lastOk = last?.stage === "main loop";
  $("dfu-boot").textContent = last
    ? `our firmware's last boot (${last.boot}) reached “${last.stage}”${lastOk ? ": it ran" : ": it stopped there"}`
    : "no boot of our firmware logged: B&W's firmware, an empty speaker, or an install erased the log";
  $("dfu-module").textContent = st.module;
  const rel = releases[0];
  const p = $("dfu-primary");
  p.replaceChildren();
  const btn = (label, primary, f) => {
    const b = el("button", { textContent: label, className: primary ? "primary" : "" });
    b.onclick = f;
    return b;
  };
  if (!last) {
    p.append(
      el("p", {
        textContent:
          "Install puts our firmware on both boards: the module's boot image, the main board's " +
          "firmware (a backup of what is there first), the module's first boot, its system. About 5 minutes; keep the " +
          "speaker plugged in. The speaker ends up without a network: the page shows where to add one.",
      }),
    );
    p.append(
      rel?.mcu && rel?.system
        ? el(
            "div",
            { className: "row" },
            btn(`Install release ${rel.version}`, true, () => installAll(rel)),
          )
        : el("p", {
            className: "warn",
            textContent: "No release with both images is offered to this page: install from files under Advanced.",
          }),
    );
  } else if (lastOk) {
    p.append(
      el("p", {
        textContent: "Our firmware ran here: it was asked into the bootloader, or ON/OFF was held at power-up.",
      }),
      el(
        "div",
        { className: "row" },
        btn("Start the firmware", true, startFirmware),
        rel?.mcu ? btn(`Install release ${rel.version}'s firmware`, false, () => installRelease(rel, "mcu")) : null,
      ),
    );
  } else {
    p.append(
      el("p", {
        textContent:
          "Our firmware was here, but its last boot stopped before it ran. Repair writes the " +
          "firmware again (a backup first); a backup below puts back what was there before.",
      }),
      el(
        "div",
        { className: "row" },
        rel?.mcu
          ? btn(`Repair: release ${rel.version}'s firmware`, true, () => installRelease(rel, "mcu"))
          : el("span", { className: "warn", textContent: "No release offered: install from a file under Advanced." }),
        btn("Start the firmware anyway", false, startFirmware),
      ),
    );
  }
  drawBackups($("dfu-backups"), restoreBackup);
}

export async function startFirmware() {
  const r = await job("Starting the firmware", false, () =>
    app.bl.start().then((r) => (r.matches ? `${r.fw} runs.` : "It started.")),
  );
  if (!r) return;
  app.bl = null;
  await connect(false);
}

/// Our firmware on both boards from the bootloader (host/core's `install`), the release's three images.
export async function installAll(rel) {
  if (
    !(await ask(
      `Install release ${rel.version}?`,
      "Both boards get our firmware; what is on the main board is " +
        "backed up first (a download, and a copy in this browser). About 5 minutes; the browser may ask once for the firmware.",
      "Install",
    ))
  )
    return;
  const r = await job(`Install release ${rel.version}`, false, async () => {
    const [mcu, system, bcd] = [
      await fetchImage(rel.mcu),
      await fetchImage(rel.system),
      rel.bcd ? await fetchImage(rel.bcd) : undefined,
    ];
    const r = await app.bl.install(bcd, rel.mcu.file, mcu, system);
    return `Done: MCU firmware ${r.fw}, system ${r.os} in slot ${r.slot.toUpperCase()}.`;
  });
  if (!r) return;
  app.bl = null;
  await connect(false);
  if (app.sp) document.querySelector("#known").scrollIntoView({ behavior: "smooth" });
}

export async function rollback() {
  if (
    !(await ask(
      "Back to B&W's software?",
      `${app.rbBcd.name} to the module, then ${app.rbMcu.name} to the main board (our firmware ` +
        "backed up first). About 7 minutes. Afterwards the speaker runs B&W's software, as from the factory, and this page " +
        "can't see it any more.",
      "Roll back",
    ))
  )
    return;
  const r = await job("Back to B&W's software", false, () =>
    app.bl
      .rollback(app.rbBcd.data, app.rbMcu.name, app.rbMcu.data)
      .then(() => "Done: the speaker starts B&W's software. Unplug it from the computer."),
  );
  if (r) {
    app.bl = null;
    show("welcome");
  }
}
// ---- software -----------------------------------------------------------------------------------------------------

export function drawSoftware(b) {
  const st = app.state,
    slot = names(app.S.Slot);
  const runs = slot[st.slot];
  const failed = ["A", "B", "the rescue"].filter((_, k) => (b.failed >> k) & 1);
  const line = (s, stampv) =>
    `${s.toUpperCase()}: ${stampv || "no system"}${runs === s ? " (runs)" : ""}${slot[st.good] === s ? " (good)" : ""}`;
  $("slots").textContent =
    [line("a", st.stamp_a), line("b", st.stamp_b)].join("; ") +
    (runs === "none" ? "; the module runs its rescue" : "") +
    (b.next ? `; the next boot tries ${slot[b.next].toUpperCase()}` : "") +
    (failed.length ? `; failed: ${failed.join(", ")}` : "");
  const back = $("ways-back");
  back.replaceChildren();
  const other = runs === "a" ? "b" : runs === "b" ? "a" : null;
  if (other && st[`stamp_${other}`]) {
    const x = el("button", {
      className: "quiet",
      textContent: `Back to slot ${other.toUpperCase()} (${st[`stamp_${other}`].split(" ")[0]})`,
    });
    x.onclick = async () => {
      if (
        !(await ask(
          `Start slot ${other.toUpperCase()}?`,
          "The module restarts with the system in the other slot; it stays there if it comes up, else it comes back to this one. Sound stops for about half a minute.",
          "Restart",
        ))
      )
        return;
      act(() =>
        app.sp
          .call("Boot", { op: "TRY", slot: other.toUpperCase(), restart: true })
          .then(() => notice(`The module restarts on slot ${other.toUpperCase()}.`)),
      );
    };
    back.append(x);
  }
  backups()
    .then((l) => {
      if (!l.length) return;
      const x = el("button", {
        className: "quiet",
        textContent: `Restore the MCU firmware ${l[0].holds} (backed up ${new Date(l[0].date).toLocaleDateString()})`,
      });
      x.onclick = () => restoreBackup(l[0]);
      back.append(x);
    })
    .catch(() => {});
}

/// The releases (images/index.json: `releases`, newest first, each with `version`, `date`, `notes` and its images
/// `mcu`, `system`, `bcd` as {file, version, size, sha256}; docs/web.md, Releases). The hosted page's come with it;
/// `zeppelin web` serves this computer's build as one release. Nothing is downloaded until it is installed.
export let releases = [];
export async function loadReleases() {
  try {
    const r = await fetch("images/index.json", { cache: "no-store" });
    if (r.ok) releases = (await r.json()).releases ?? [];
  } catch {
    releases = [];
  }
  app.drawnFor = "";
  if (app.sp && app.info) drawReleases();
  if (app.bl) drawDfu();
}

/// A release's image, fetched and checked against the manifest's SHA-256.
export async function fetchImage(img) {
  const r = await fetch(`images/${img.file}`, { cache: "no-store" });
  if (!r.ok) throw new Error(`${img.file}: ${r.status} ${r.statusText}`);
  const data = new Uint8Array(await r.arrayBuffer());
  if (img.sha256) {
    const h = [...new Uint8Array(await crypto.subtle.digest("SHA-256", data))]
      .map((b) => b.toString(16).padStart(2, "0"))
      .join("");
    if (h !== img.sha256) throw new Error(`${img.file}: not the file the release lists (its SHA-256 differs)`);
  }
  return data;
}

/// Where a running version stands against the releases: its position in the list (versions are commits: they don't
/// order), -1 for a build the list doesn't have.
export const position = (running, kind) => (running ? releases.findIndex((r) => r[kind]?.version === running) : -1);

/// The banner on top: what there is to do, with the one button.
function banner(title, text, label, f, notes) {
  const u = el("button", { className: "primary", textContent: label });
  u.onclick = f;
  $("update").replaceChildren(
    el("i", { className: "lamp on" }),
    el(
      "div",
      { className: "banner-text" },
      el("strong", { textContent: title }),
      el("span", { className: "small", textContent: text }),
      notes
        ? el(
            "details",
            {},
            el("summary", { textContent: "What's new" }),
            el("p", { className: "small notes", textContent: notes }),
          )
        : null,
    ),
    u,
  );
  $("update").hidden = false;
}

/// An install that stopped on the way (the person away, the page closed): the state it left, picked up from there.
/// Our firmware with the module on the stock software (the boot image never went on): the whole install again. The
/// module on its rescue: it has no system yet (or none that starts): the system.
function unfinished(rel) {
  const ms = names(app.S.ModState)[app.info.mod_state];
  if (ms === "stock" && rel.mcu && rel.system && rel.bcd) {
    banner(
      "The install isn't finished",
      "The module still runs B&W's software: its boot image, then the system go on (about 5 minutes).",
      `Finish: install ${rel.version}`,
      () => finishAll(rel),
    );
    return true;
  }
  if (ms === "ready" && app.info.mod_os?.split(" ")[2] === "rescue" && rel.system) {
    banner(
      "The install isn't finished",
      "The module runs its rescue: it has no system yet. The system goes on (about 2 minutes).",
      `Finish: install ${rel.version}'s system`,
      () => updateTo(rel, ["system"], true),
    );
    return true;
  }
  return false;
}

async function finishAll(rel) {
  const r = await job(`Install release ${rel.version}`, false, async () => {
    const [mcu, system, bcd] = [await fetchImage(rel.mcu), await fetchImage(rel.system), await fetchImage(rel.bcd)];
    const sp = app.sp;
    app.sp = null; // the install takes the speaker into its bootloader: this connection ends
    const r = await sp.install(bcd, rel.mcu.file, mcu, system);
    return `Done: MCU firmware ${r.fw}, system ${r.os} in slot ${r.slot.toUpperCase()}.`;
  });
  await connect(false);
  if (r && app.sp) document.querySelector("#known").scrollIntoView({ behavior: "smooth" });
}

export function drawReleases() {
  const fw = app.info?.fw,
    os = app.info?.mod_os?.split(" ")[1];
  app.drawnFor = `${app.info?.fw}|${app.info?.mod_os}|${app.info?.mod_state}`;
  const v = $("verdict");
  v.replaceChildren();
  $("update").hidden = true;
  const rel = releases[0];
  if (rel && unfinished(rel)) {
    v.append(el("p", { textContent: "The install stopped on the way: the banner on top finishes it." }));
  } else if (!rel) {
    v.append(
      el("p", { className: "small", textContent: "No release is offered to this page (installs from a file below)." }),
    );
  } else {
    const [pm, ps] = [position(fw, "mcu"), position(os, "system")];
    const say = (p, what, running) =>
      p === 0
        ? `${what} up to date`
        : p > 0
          ? `${what} ${p} release${p > 1 ? "s" : ""} behind`
          : running
            ? `${what} on ${running}, a build that isn't a release`
            : `${what} without our software`;
    const needs = [rel.system && ps !== 0 && "system", rel.mcu && pm !== 0 && "mcu"].filter(Boolean);
    const where = `${say(pm, "main board", fw)}, ${say(ps, "module", os)}.`;
    const named = `release ${rel.version}${rel.date ? ` (${rel.date})` : ""}`;
    v.append(el("p", { textContent: needs.length ? `Latest ${named}: ${where}` : `Up to date: ${named}.` }));
    if (rel.notes) v.append(el("p", { className: "small notes", textContent: rel.notes }));
    // behind: the banner on top of the page says so, with the one button
    if (needs.length)
      banner(
        `Release ${rel.version} is available`,
        where[0].toUpperCase() + where.slice(1),
        `Update to ${rel.version}`,
        () => updateTo(rel, needs),
        rel.notes,
      );
  }
  const rows = releases.slice(1).map((r) => {
    const row = el(
      "div",
      { className: "row item" },
      el("span", { textContent: `${r.version}${r.date ? `, ${r.date}` : ""}` }),
      el("span", { className: "grow" }),
    );
    if (r.notes) row.title = r.notes;
    for (const [kind, label] of [
      ["system", "its system"],
      ["mcu", "its MCU firmware"],
    ]) {
      if (!r[kind]) continue;
      const b = el("button", { className: "quiet", textContent: `Install ${label}` });
      b.onclick = () => installRelease(r, kind);
      row.append(b);
    }
    return row;
  });
  const d = el(
    "details",
    {},
    el("summary", { className: "small", textContent: `${rows.length} earlier release${rows.length === 1 ? "" : "s"}` }),
    ...rows,
  );
  $("builds").replaceChildren(...(rows.length ? [d] : []));
  $("builds").hidden = !rows.length;
}

/// Both boards to a release: the system, then the MCU firmware (either order works; this one keeps the page
/// connected longest), each only if it isn't there already.
export async function updateTo(rel, needs, finishing = false) {
  if (
    !finishing &&
    !(await ask(
      `Update to ${rel.version}?`,
      `${needs.includes("system") ? "The module's system goes into the slot that doesn't run (the old one stays as the way back)" : ""}` +
        `${needs.length === 2 ? "; then " : ""}${needs.includes("mcu") ? "the main board's firmware (a backup of it first: a download, and a copy in this browser)" : ""}` +
        ". A few minutes; sound stops meanwhile.",
      "Update",
    ))
  )
    return;
  const r = await job(`Update to ${rel.version}`, false, async () => {
    const said = [];
    if (needs.includes("system")) {
      events.phase(1, needs.length, "the module's system");
      const r = await app.sp.updateSystem(await fetchImage(rel.system), true);
      if (r.outcome === "fellBack")
        throw new Error(
          `The system in slot ${r.slot.toUpperCase()} didn't come up: the module is back on slot ${r.now.toUpperCase()}. Nothing else was changed.`,
        );
      said.push(`system ${rel.system.version} in slot ${r.slot.toUpperCase()}`);
    }
    if (needs.includes("mcu")) {
      events.phase(needs.length, needs.length, "the main board's firmware");
      const r = await app.sp.flashMcu(rel.mcu.file, await fetchImage(rel.mcu));
      app.sp = null;
      said.push(r.matches ? `MCU firmware ${r.fw}` : `MCU firmware: ${r.fw} runs, not the one written`);
    }
    return `Done: ${said.join(", ")}.`;
  });
  if (!app.sp) await connect(false);
  else refresh(true);
  return r;
}

export async function installRelease(rel, kind) {
  let data;
  try {
    data = await fetchImage(rel[kind]);
  } catch (e) {
    return failed(e);
  }
  return kind === "system" ? installSystem(rel[kind].file, data) : installMcu(rel[kind].file, data);
}

export async function installSystem(name, data) {
  let i;
  try {
    i = imageInfo(name, data);
  } catch (e) {
    return failed(e);
  }
  if (i.kind !== "system") return notice(`${name} is not a system image (a gzip'd cpio).`, "warn");
  if (
    !(await ask(
      "Install the module's system?",
      `${name}: version ${i.version}, ${(i.size / 1048576).toFixed(1)} MB. It goes ` +
        "into the slot that doesn't run, then the module starts it once: kept if it comes up, else the old one comes back. " +
        "About a minute and a half; sound stops while the module restarts.",
      "Install",
    ))
  )
    return;
  await job(`System ${i.version}`, true, async () => {
    const r = await app.sp.updateSystem(data, true);
    return r.outcome === "running"
      ? `Done: slot ${r.slot.toUpperCase()} runs ${i.version}.`
      : r.outcome === "installed"
        ? `Installed in slot ${r.slot.toUpperCase()}.`
        : `Slot ${r.slot.toUpperCase()} didn't come up: the module is back on slot ${r.now.toUpperCase()}.`;
  });
  refresh(true);
}

/// MCU firmware from the running firmware or the bootloader: a .hex, the app region's .bin, or a backup (a download,
/// or one this browser kept).
export async function installMcu(name, data) {
  let i;
  try {
    i = imageInfo(name, data);
  } catch (e) {
    return failed(e);
  }
  if (i.kind !== "mcu" && i.kind !== "backup")
    return notice(`${name} is not MCU firmware (Intel HEX, the app region's .bin, or a backup).`, "warn");
  const from = i.kind === "backup" ? ` (the backup read ${new Date(i.when).toLocaleString()})` : "";
  if (
    !(await ask(
      "Install the MCU firmware?",
      `${name}${from}: ${i.version ?? "no version in it (B&W's firmware, or a backup of it)"}${i.built ? `, built ${i.built}` : ""}. ` +
        (app.bl ? "" : "The speaker restarts into its bootloader; ") +
        "this page saves a backup of the firmware on it (a download, and a copy in this browser) before anything is written, " +
        "then writes, checks and starts the new one. About 20 s; the browser may ask once for the bootloader or the firmware.",
      "Install",
    ))
  )
    return;
  const target = app.sp ?? app.bl;
  const r = await job(`MCU firmware ${i.version ?? ""}`, false, () =>
    target
      .flashMcu(name, data)
      .then((r) =>
        r.matches
          ? `Done: ${r.fw} runs.`
          : r.fw
            ? `It runs ${r.fw}, not the version just written.`
            : "Written and started.",
      ),
  );
  app.sp = null;
  app.bl = null;
  if (r) await connect(false);
}
