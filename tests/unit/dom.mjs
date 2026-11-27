export class Element extends EventTarget {
  classList = { add() {}, remove() {} };
  style = {};
  childNodes = [];
  attributes = new Map();

  constructor(tagName) {
    super();
    this.tagName = tagName;
  }

  get firstElementChild() { return this.childNodes.find((node) => node instanceof Element); }
  append(...nodes) { this.childNodes.push(...nodes); }
  replaceChildren(...nodes) { this.childNodes = nodes; }
  setAttribute(name, value) { this.attributes.set(name, String(value)); }
  getAttribute(name) { return this.attributes.get(name) ?? null; }
  setSelectionRange() {}
  focus() {}
  blur() {}
}

export function dispatch(target, type, fields) {
  target.dispatchEvent(Object.assign(new Event(type, { cancelable: true }), fields));
}
