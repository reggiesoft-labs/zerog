// Replaces a native <select>'s popup with a styleable listbox. The native
// select stays in the DOM (hidden) as the source of truth: picking an option
// sets select.value and dispatches "change", so existing listeners keep
// working. Call sync() after setting select.value programmatically.

const ENHANCED_KEY = "__mouseThingCustomSelect";
const MENU_MAX_HEIGHT = 256;
const MENU_GAP = 4;
const VIEWPORT_MARGIN = 8;

let nextId = 0;

export function enhanceSelect(select) {
  if (!select) {
    return null;
  }
  if (select[ENHANCED_KEY]) {
    select[ENHANCED_KEY].rebuild();
    return select[ENHANCED_KEY];
  }

  const id = select.id || `custom-select-${++nextId}`;
  const wrapper = document.createElement("div");
  wrapper.className = "custom-select";

  const button = document.createElement("button");
  button.type = "button";
  button.id = `${id}-button`;
  button.className = "setting-select custom-select__button";
  button.setAttribute("aria-haspopup", "listbox");
  button.setAttribute("aria-expanded", "false");

  const menu = document.createElement("ul");
  menu.id = `${id}-menu`;
  menu.className = "custom-select__menu";
  menu.setAttribute("role", "listbox");
  menu.tabIndex = -1;
  menu.hidden = true;
  button.setAttribute("aria-controls", menu.id);

  select.parentNode.insertBefore(wrapper, select);
  wrapper.append(select, button, menu);
  select.classList.add("custom-select__native");
  select.tabIndex = -1;
  select.setAttribute("aria-hidden", "true");

  for (const label of document.querySelectorAll(`label[for="${select.id}"]`)) {
    label.htmlFor = button.id;
    button.setAttribute("aria-labelledby", label.id || (label.id = `${id}-label`));
  }

  let activeIndex = -1;

  function items() {
    return Array.from(menu.children);
  }

  function rebuild() {
    menu.replaceChildren(
      ...Array.from(select.options).map((option, index) => {
        const item = document.createElement("li");
        item.id = `${id}-option-${index}`;
        item.className = "custom-select__option";
        item.setAttribute("role", "option");
        item.dataset.value = option.value;
        item.textContent = option.textContent;
        return item;
      }),
    );
    sync();
  }

  function sync() {
    const selected = select.options[select.selectedIndex];
    button.textContent = selected ? selected.textContent : "";
    items().forEach((item, index) => {
      item.setAttribute("aria-selected", String(index === select.selectedIndex));
    });
  }

  function setActive(index) {
    const list = items();
    if (list.length === 0) {
      return;
    }
    activeIndex = Math.max(0, Math.min(list.length - 1, index));
    list.forEach((item, i) => item.classList.toggle("is-active", i === activeIndex));
    const active = list[activeIndex];
    menu.setAttribute("aria-activedescendant", active.id);
    active.scrollIntoView?.({ block: "nearest" });
  }

  function position() {
    const rect = button.getBoundingClientRect();
    const below = window.innerHeight - rect.bottom - MENU_GAP - VIEWPORT_MARGIN;
    const above = rect.top - MENU_GAP - VIEWPORT_MARGIN;
    const openUp = below < Math.min(MENU_MAX_HEIGHT, menu.scrollHeight) && above > below;
    menu.classList.toggle("custom-select__menu--up", openUp);
    menu.style.maxHeight = `${Math.max(120, Math.min(MENU_MAX_HEIGHT, openUp ? above : below))}px`;
  }

  function isOpen() {
    return !menu.hidden;
  }

  function open() {
    if (isOpen() || select.disabled) {
      return;
    }
    menu.hidden = false;
    button.setAttribute("aria-expanded", "true");
    wrapper.classList.add("is-open");
    position();
    setActive(select.selectedIndex);
    menu.focus({ preventScroll: true });
    document.addEventListener("pointerdown", handleOutsidePointer, true);
  }

  function close({ focusButton = true } = {}) {
    if (!isOpen()) {
      return;
    }
    menu.hidden = true;
    button.setAttribute("aria-expanded", "false");
    wrapper.classList.remove("is-open");
    menu.removeAttribute("aria-activedescendant");
    document.removeEventListener("pointerdown", handleOutsidePointer, true);
    if (focusButton) {
      button.focus({ preventScroll: true });
    }
  }

  function choose(index) {
    const option = select.options[index];
    close();
    if (!option || index === select.selectedIndex) {
      return;
    }
    select.selectedIndex = index;
    sync();
    select.dispatchEvent(new Event("change", { bubbles: true }));
  }

  function handleOutsidePointer(event) {
    if (!wrapper.contains(event.target)) {
      close({ focusButton: false });
    }
  }

  function handleKeys(event) {
    const count = select.options.length;
    switch (event.key) {
      case "ArrowDown":
        event.preventDefault();
        if (!isOpen()) open();
        else setActive(activeIndex + 1);
        break;
      case "ArrowUp":
        event.preventDefault();
        if (!isOpen()) open();
        else setActive(activeIndex - 1);
        break;
      case "Home":
        if (isOpen()) {
          event.preventDefault();
          setActive(0);
        }
        break;
      case "End":
        if (isOpen()) {
          event.preventDefault();
          setActive(count - 1);
        }
        break;
      case "Enter":
      case " ":
        event.preventDefault();
        if (isOpen()) choose(activeIndex);
        else open();
        break;
      case "Escape":
        if (isOpen()) {
          event.preventDefault();
          event.stopPropagation();
          close();
        }
        break;
      case "Tab":
        close({ focusButton: false });
        break;
      default:
        break;
    }
  }

  button.addEventListener("click", () => (isOpen() ? close() : open()));
  button.addEventListener("keydown", handleKeys);
  menu.addEventListener("keydown", handleKeys);
  menu.addEventListener("focusout", (event) => {
    // relatedTarget is null when clicking a non-focusable spot or a button on
    // macOS WebKit; the outside-pointer handler covers those cases.
    if (event.relatedTarget && !wrapper.contains(event.relatedTarget)) {
      close({ focusButton: false });
    }
  });
  menu.addEventListener("pointermove", (event) => {
    const item = event.target.closest?.(".custom-select__option");
    if (item) setActive(items().indexOf(item));
  });
  menu.addEventListener("click", (event) => {
    const item = event.target.closest?.(".custom-select__option");
    if (item) choose(items().indexOf(item));
  });
  select.addEventListener("change", sync);

  const api = { sync, rebuild, close };
  select[ENHANCED_KEY] = api;
  rebuild();
  return api;
}
