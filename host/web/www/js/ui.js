// The page's small tools: elements, notices, the confirm dialog, downloads and file inputs.

export const $ = (id) => document.getElementById(id);
export const el = (tag, props = {}, ...kids) => {
  const e = Object.assign(document.createElement(tag), props);
  e.append(...kids.filter((k) => k !== null && k !== undefined && k !== false));
  return e;
};
export const names = (e) => Object.fromEntries(Object.entries(e).map(([k, v]) => [v, k.toLowerCase()]));
export const hex4 = (n) => Number(n).toString(16).toUpperCase().padStart(4, "0");
/// A slider's groove filled up to its value (style.css: --fill).
export const fill = (r) => r.style.setProperty("--fill", `${((r.value - r.min) / (r.max - r.min)) * 100}%`);
/// A segmented control into `into`: `choices` [key, label, lamp colour?], the one pressed in, what a pick does.
export function segmented(into, choices, current, pick) {
  into.replaceChildren(
    ...choices.map(([key, label, lamp]) => {
      const b = el("button", { type: "button", textContent: label });
      b.setAttribute("aria-pressed", String(key === current));
      if (lamp) b.style.setProperty("--lamp", lamp);
      b.onclick = () => pick(key);
      return b;
    }),
  );
}
export const stamp = () => new Date().toISOString().replace(/[-:]/g, "").replace("T", "-").slice(0, 15);

// ---- notices: every outcome in the page, never alert() ----------------------------------------------------------

export function notice(text, kind = "info") {
  const n = el(
    "div",
    { className: `notice ${kind}` },
    el("span", { textContent: text }),
    el("span", { className: "grow" }),
  );
  const x = el("button", { className: "quiet", textContent: "Dismiss" });
  x.onclick = () => n.remove();
  n.append(x);
  $("notices").prepend(n);
  if (kind !== "warn") setTimeout(() => n.remove(), 12000);
}

export function failed(e) {
  if (e?.code === "CANCELLED") return;
  if (e?.code === "DENIED") return notice("Nobody pressed ON/OFF on the speaker in time: nothing changed.", "warn");
  notice(e?.message ?? String(e), "warn");
}
export function ask(title, text, yes = "Go ahead") {
  $("confirm-title").textContent = title;
  $("confirm-text").textContent = text;
  $("confirm-yes").textContent = yes;
  $("confirm").showModal();
  return new Promise((res) => {
    $("confirm-yes").onclick = () => {
      $("confirm").close();
      res(true);
    };
    $("confirm-no").onclick = () => {
      $("confirm").close();
      res(false);
    };
  });
}
export function download(name, blob) {
  const a = el("a", { href: URL.createObjectURL(blob), download: name });
  a.click();
  setTimeout(() => URL.revokeObjectURL(a.href), 10000);
}

export const readFile = async (input) => {
  const f = input.files[0];
  input.value = "";
  return f ? { name: f.name, data: new Uint8Array(await f.arrayBuffer()) } : null;
};
