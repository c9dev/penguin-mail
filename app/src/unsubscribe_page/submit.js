// Carries out one plan on the page the extraction script already read,
// finding each control by the id that script left on it. A box is set
// rather than toggled, so running the same plan twice leaves the page
// where running it once did. The answer names whatever the plan asked
// for that the page no longer holds, and nothing is pressed when
// anything is missing.
(json) => {
  const plan = JSON.parse(json);
  const find = (id) => document.querySelector(`[data-pm-id="${id}"]`);
  const missing = [];

  // Pages built on a framework watch the property rather than the
  // attribute, so the value goes in through the prototype's own setter
  // and the events a person typing would raise follow it.
  const type = (element, text) => {
    const shape =
      element.tagName.toLowerCase() === "textarea"
        ? HTMLTextAreaElement.prototype
        : HTMLInputElement.prototype;
    const setter = Object.getOwnPropertyDescriptor(shape, "value");
    if (setter && setter.set) {
      setter.set.call(element, text);
    } else {
      element.value = text;
    }
    element.dispatchEvent(new Event("input", { bubbles: true }));
    element.dispatchEvent(new Event("change", { bubbles: true }));
  };

  for (const [id, text] of plan.fill) {
    const element = find(id);
    if (!element) {
      missing.push(id);
      continue;
    }
    element.focus();
    type(element, text);
  }

  for (const id of plan.tick) {
    const element = find(id);
    if (!element) {
      missing.push(id);
      continue;
    }
    if (!element.checked) {
      element.checked = true;
      element.dispatchEvent(new Event("input", { bubbles: true }));
      element.dispatchEvent(new Event("change", { bubbles: true }));
    }
  }

  const button = find(plan.press);
  if (!button) {
    missing.push(plan.press);
  }
  if (missing.length) {
    return JSON.stringify({ missing });
  }
  button.click();
  return JSON.stringify({ missing: [] });
}
