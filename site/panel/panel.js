"use strict";

// パネルのプレビュー。キオスクには接続せず、4 / 6 で画面の巡回、5 で状況画面のページ切替を模す。
(() => {
  const screens = [
    { id: "status", pages: ["OVERVIEW", "DETAILS"] },
    { id: "photo", pages: null },
    { id: "agent", pages: ["LIST"] },
  ];
  const device = document.querySelector("[data-panel]");
  const strip = [...document.querySelectorAll("#screen-strip li")];
  const detail = document.getElementById("panel-detail");
  const photo = document.getElementById("panel-photo");
  const selectLabel = document.getElementById("select-label");
  const keys = [...document.querySelectorAll("[data-key]")];
  const clock = document.getElementById("panel-clock");
  let index = 0;
  let page = 0;
  let position = 12;
  const total = 187;

  function render() {
    strip.forEach((li, i) => li.classList.toggle("is-current", i === index));
    const s = screens[index];
    if (s.pages) {
      detail.textContent = s.pages[page % s.pages.length];
      detail.hidden = false;
      photo.hidden = true;
      selectLabel.textContent = s.pages.length > 1 ? "ページ切替" : "詳細 / 決定";
    } else {
      detail.hidden = true;
      photo.hidden = false;
      photo.textContent = `写真 ${position} / ${total}`;
      selectLabel.textContent = "情報帯の表示";
    }
  }

  function act(key) {
    if (key === "prev") index = (index + screens.length - 1) % screens.length;
    else if (key === "next") index = (index + 1) % screens.length;
    else if (key === "select") {
      if (screens[index].pages) page += 1;
      else position = (position % total) + 1;
    }
    render();
    const button = keys.find((k) => k.dataset.key === key);
    if (button) {
      button.classList.add("is-pressed");
      setTimeout(() => button.classList.remove("is-pressed"), 120);
    }
  }

  keys.forEach((k) => k.addEventListener("click", () => act(k.dataset.key)));
  if (device) {
    device.addEventListener("keydown", (ev) => {
      const map = { 4: "prev", 6: "next", 5: "select", ArrowLeft: "prev", ArrowRight: "next" };
      const key = map[ev.key];
      if (key) {
        ev.preventDefault();
        act(key);
      }
    });
  }
  if (clock) {
    const tick = () => {
      const now = new Date();
      clock.textContent = `${String(now.getHours()).padStart(2, "0")}:${String(now.getMinutes()).padStart(2, "0")}`;
    };
    tick();
    setInterval(tick, 30000);
  }
  render();
})();
