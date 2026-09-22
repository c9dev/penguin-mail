// Turns the page the hidden view is showing into the data a `PageForm`
// holds, and leaves every control marked with the id it was given so the
// submission script finds it again. The answer is one JSON string and
// never any markup, so nothing a sender wrote reaches the rules or a
// model as something to read.
(() => {
  // What the Rust side cuts these to, repeated here so a page that runs
  // to thousands of words hands back the same string either way.
  const LABEL = 80;
  const TITLE = 120;
  const TEXT = 2000;
  const FORMS = 5;

  // The loose half of `words::leave`, folded the same way. It decides
  // only whether a link or a stray button is worth reporting at all;
  // `words::presses` decides what may be pressed, and it sees the label
  // in full.
  const LEAVING = [
    "unsubscribe",
    "opt out",
    "remove me",
    "stop receiving",
    "leave this list",
    "manage preferences",
    "manage subscriptions",
    "email preferences",
    "cancelar subscricao",
    "cancelar a subscricao",
    "cancelar todas as subscricoes",
    "anular subscricao",
    "anular todas as subscricoes",
    "deixar de receber",
    "gerir preferencias",
    "preferencias de email",
  ];

  const CAPTCHAS = [
    'iframe[src*="recaptcha"]',
    'iframe[src*="hcaptcha"]',
    'iframe[src*="turnstile"]',
    'iframe[src*="captcha"]',
    ".g-recaptcha",
    ".h-captcha",
    ".cf-turnstile",
    "[data-sitekey]",
  ].join(",");

  const CONTROLS = ["input", "select", "textarea", "button"];

  const fold = (text) =>
    (text || "")
      .toLowerCase()
      .normalize("NFD")
      .replace(/[̀-ͯ]/g, "")
      .replace(/[^a-z0-9]+/g, " ")
      .trim();

  const leaves = (text) => {
    const folded = fold(text);
    return LEAVING.some((word) => folded.includes(word));
  };

  const cut = (text, limit) => {
    const tidy = (text || "").replace(/\s+/g, " ").trim();
    return tidy.length > limit ? tidy.slice(0, limit) : tidy;
  };

  let next = 0;
  const mark = (element) => {
    const id = next++;
    element.setAttribute("data-pm-id", String(id));
    return id;
  };

  // Whether a person looking at the page would see this control.
  // Computed style answers on its own, without the layout a view that
  // was never put on screen has no size to work out.
  const shown = (element) => {
    if (element.hidden || element.getAttribute("aria-hidden") === "true") {
      return false;
    }
    for (let node = element; node && node.nodeType === 1; node = node.parentElement) {
      const style = window.getComputedStyle(node);
      if (!style) {
        return true;
      }
      if (style.display === "none" || style.visibility === "hidden") {
        return false;
      }
    }
    return true;
  };

  // What a person reads as this field's name: the accessible name if the
  // page gives one, then the label that points at it or wraps it, then
  // what the box says while it is empty, and last the words just before
  // it.
  const labelOf = (element) => {
    const aria = element.getAttribute("aria-label");
    if (aria && aria.trim()) {
      return cut(aria, LABEL);
    }
    const names = element.getAttribute("aria-labelledby");
    if (names) {
      const named = names
        .split(/\s+/)
        .map((id) => document.getElementById(id))
        .filter((node) => node)
        .map((node) => node.textContent)
        .join(" ");
      if (named.trim()) {
        return cut(named, LABEL);
      }
    }
    if (element.id) {
      const tag = document.querySelector(`label[for="${CSS.escape(element.id)}"]`);
      if (tag && tag.textContent.trim()) {
        return cut(tag.textContent, LABEL);
      }
    }
    const around = element.closest("label");
    if (around && around.textContent.trim()) {
      return cut(around.textContent, LABEL);
    }
    for (const attribute of ["placeholder", "title"]) {
      const said = element.getAttribute(attribute);
      if (said && said.trim()) {
        return cut(said, LABEL);
      }
    }
    // The words just before the box name it, as long as nothing else
    // that takes an answer stands between. A label that comes before
    // another field belongs to that field.
    for (let node = element.previousSibling; node; node = node.previousSibling) {
      if (node.nodeType === 1 && node.querySelector("input, select, textarea, button")) {
        break;
      }
      if (node.nodeType === 1 && CONTROLS.includes(node.tagName.toLowerCase())) {
        break;
      }
      const text = (node.textContent || "").trim();
      if (text) {
        return cut(text, LABEL);
      }
    }
    // The words around a box that holds nothing else name it. Where the
    // parent holds several controls they name none of them in
    // particular, and the field stays unlabelled, which is what makes
    // the rules give up on a page that insists on it.
    const parent = element.parentElement;
    if (parent && parent.querySelectorAll("input, select, textarea, button").length === 1) {
      return cut(parent.textContent, LABEL);
    }
    return "";
  };

  const labelOfButton = (element) => {
    const aria = element.getAttribute("aria-label");
    if (aria && aria.trim()) {
      return cut(aria, LABEL);
    }
    const text = (element.textContent || "").trim();
    if (text) {
      return cut(text, LABEL);
    }
    for (const attribute of ["value", "alt", "title"]) {
      const said = element.getAttribute(attribute);
      if (said && said.trim()) {
        return cut(said, LABEL);
      }
    }
    return "";
  };

  const kindOf = (element) => {
    const tag = element.tagName.toLowerCase();
    if (tag === "select") {
      const options = Array.from(element.options).map((option) =>
        cut(option.textContent || option.value, LABEL),
      );
      return { select: { options } };
    }
    if (tag === "textarea") {
      return "text";
    }
    const type = (element.getAttribute("type") || "text").toLowerCase();
    if (type === "email") {
      return "email";
    }
    if (type === "password") {
      return "password";
    }
    if (type === "hidden") {
      return "hidden";
    }
    if (type === "checkbox") {
      return "checkbox";
    }
    if (type === "radio") {
      return { radio: { group: element.name || "" } };
    }
    return "text";
  };

  const fieldOf = (element) => {
    const kind = kindOf(element);
    const boxed = kind === "checkbox" || (kind && kind.radio);
    return {
      id: mark(element),
      kind,
      label: labelOf(element),
      // A password's own value never leaves the page, and a box says
      // what it holds by being ticked.
      value: kind === "password" || boxed ? "" : cut(element.value || "", LABEL),
      checked: boxed ? !!element.checked : false,
      required: !!element.required || element.getAttribute("aria-required") === "true",
    };
  };

  const PRESSES = ["submit", "button", "image"];

  // The fields and buttons of one form, numbered as they stand in the
  // page, so a form reads back in the order a person sees it.
  const controlsOf = (form) => {
    const fields = [];
    const buttons = [];
    for (const element of form.querySelectorAll("input, select, textarea, button")) {
      if (element.disabled) {
        continue;
      }
      const tag = element.tagName.toLowerCase();
      const type = (element.getAttribute("type") || "").toLowerCase();
      if (type === "reset") {
        continue;
      }
      if (tag === "button" || (tag === "input" && PRESSES.includes(type))) {
        buttons.push({ id: mark(element), label: labelOfButton(element) });
        continue;
      }
      if (type !== "hidden" && !shown(element)) {
        continue;
      }
      fields.push(fieldOf(element));
    }
    return { fields, buttons };
  };

  const forms = [];
  for (const form of Array.from(document.forms).slice(0, FORMS)) {
    const id = mark(form);
    const { fields, buttons } = controlsOf(form);
    forms.push({ id, fields, buttons });
  }

  // Plenty of these pages are one link and nothing else. Where no form
  // already offers a way off the list, the links and stray buttons that
  // read as one become a form of their own with nothing to fill in.
  const offered = forms.some((form) => form.buttons.some((button) => leaves(button.label)));
  if (!offered && forms.length < FORMS) {
    const id = next++;
    const buttons = [];
    for (const element of document.querySelectorAll("a[href], button")) {
      if (element.closest("form") || element.disabled || !shown(element)) {
        continue;
      }
      const label = labelOfButton(element);
      if (leaves(label)) {
        buttons.push({ id: mark(element), label });
      }
    }
    if (buttons.length) {
      forms.push({ id, fields: [], buttons });
    }
  }

  // A page that was never put on screen has no layout to read visible
  // text from, so the whole text stands in when innerText comes back
  // empty.
  const body = document.body;
  const laid = body ? body.innerText || "" : "";
  const whole = laid.trim() ? laid : body ? body.textContent || "" : "";
  const text = whole
    .split("\n")
    .map((line) => line.replace(/\s+/g, " ").trim())
    .filter((line) => line)
    .join("\n")
    .slice(0, TEXT);

  return JSON.stringify({
    url: location.href,
    title: cut(document.title, TITLE),
    text,
    forms,
    captcha: !!document.querySelector(CAPTCHAS),
    password: !!document.querySelector('input[type="password"]'),
  });
})();
