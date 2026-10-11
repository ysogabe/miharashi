"use strict";

(() => {
  const definitions = [
    {
      name: "システム監視", code: "SYSTEM", title: "数値と履歴を、同じ視線の中に。",
      description: "CPU・GPU・メモリ・VRAMのサマリーと時系列グラフ。温度や電力は取得元とともに表示し、欠損や古いデータを区別します。",
      items: ["サマリーカード", "CPU / GPU履歴", "温度・電力・データ状態"],
      rows: [["上部", "ホスト名、時刻、画面名、データ状態"], ["中央", "CPU / 選択したGPU / メモリ / VRAMカード"], ["下部", "時系列グラフと表示デバイス・監視対象GPUの識別"], ["詳細", "取得元、更新時刻、センサー名、N/AやSTALEの理由"]]
    },
    {
      name: "エージェント監視", code: "AGENTS", title: "いま動いているものを、見渡せる。",
      description: "名前・状態・最終更新を一覧に。Hermes、OpenClaw、Codex、Claude CodeのモックAdapterから始め、取得元を交換して連携を追加します。",
      items: ["名前と状態の一覧", "稼働・待機・応答待ちの集計", "Adapterとデータの鮮度"],
      rows: [["上部", "Adapter種別、取得時刻、状態別の件数"], ["中央", "エージェント名、状態、短い作業概要、最終更新"], ["詳細", "選択エージェントの補足と取得元"], ["MVP", "MockAgentAdapterを使用。実製品との接続は未実装"]]
    },
    {
      name: "フォトフレーム", code: "PHOTO", title: "写真を主役に、情報をそっと重ねる。",
      description: "写真を画面いっぱいに表示し、日時や小さな監視情報をオーバーレイ。MVPは設定したローカルの1枚を使い、未設定・読込失敗も表示します。",
      items: ["JPEG / PNG写真の表示", "日時・任意の文章", "小さなシステム情報の重ね合わせ"],
      rows: [["背景", "写真をcover / containで表示"], ["上部", "日時と画面名を表示"], ["下部", "短い文章、CPU/メモリ等の状態を重ねる"], ["MVP", "指定した1枚。写真未設定時はプレースホルダー"]]
    }
  ];

  const root = document.querySelector("[data-demo]");
  const controls = document.querySelector(".demo-controls");
  const tabs = [...document.querySelectorAll("[data-screen]")];
  const panels = [...document.querySelectorAll(".preview-panel")];
  const menu = document.getElementById("menu-panel");
  const detail = document.getElementById("detail-panel");
  const menuButtons = [...document.querySelectorAll("[data-menu]")];
  const actionButtons = [...document.querySelectorAll("[data-action]")];
  let index = 0;
  let menuIndex = 0;
  let returnFocus = null;

  const wrap = value => (value + definitions.length) % definitions.length;

  function renderMenu() {
    menuButtons.forEach((button, position) => button.classList.toggle("is-selected", position === menuIndex));
  }

  function closeOverlay(restoreFocus = true) {
    menu.hidden = true;
    detail.hidden = true;
    actionButtons.filter(button => button.hasAttribute("aria-expanded")).forEach(button => button.setAttribute("aria-expanded", "false"));
    if (restoreFocus && returnFocus && returnFocus.isConnected) returnFocus.focus({preventScroll: true});
    returnFocus = null;
  }

  function selectScreen(value, focusTab = false) {
    index = wrap(value);
    const page = definitions[index];
    tabs.forEach((tab, position) => {
      tab.setAttribute("aria-selected", String(position === index));
      tab.tabIndex = position === index ? 0 : -1;
    });
    panels.forEach((panel, position) => { panel.hidden = position !== index; });
    document.getElementById("active-label").textContent = `0${index + 1} / ${page.name}`;
    document.getElementById("composition-index").textContent = `0${index + 1} / ${page.code}`;
    document.getElementById("composition-title").textContent = page.title;
    document.getElementById("composition-text").textContent = page.description;
    const list = document.getElementById("composition-items");
    list.replaceChildren(...page.items.map(text => {
      const item = document.createElement("li");
      item.textContent = text;
      return item;
    }));
    closeOverlay(false);
    if (focusTab) tabs[index].focus({preventScroll: true});
  }

  function toggleMenu(trigger) {
    if (!menu.hidden) { closeOverlay(); return; }
    closeOverlay(false);
    returnFocus = trigger || document.activeElement;
    menuIndex = index;
    renderMenu();
    menu.hidden = false;
    document.querySelector('[data-action="menu"]').setAttribute("aria-expanded", "true");
    menuButtons[menuIndex].focus({preventScroll: true});
  }

  function toggleDetail(trigger) {
    if (!detail.hidden) { closeOverlay(); return; }
    closeOverlay(false);
    returnFocus = trigger || document.activeElement;
    const page = definitions[index];
    document.getElementById("detail-title").textContent = `${page.name}の画面構成`;
    document.getElementById("detail-description").textContent = page.description;
    const list = document.getElementById("detail-list");
    list.replaceChildren(...page.rows.flatMap(([label, text]) => {
      const term = document.createElement("dt");
      const value = document.createElement("dd");
      term.textContent = label;
      value.textContent = text;
      return [term, value];
    }));
    detail.hidden = false;
    document.querySelector('[data-action="detail"]').setAttribute("aria-expanded", "true");
    detail.querySelector("[data-close]").focus({preventScroll: true});
  }

  function action(name, trigger) {
    if (name === "menu") { toggleMenu(trigger); return; }
    if (name === "detail" && !menu.hidden) {
      const focusTarget = returnFocus;
      selectScreen(menuIndex);
      if (focusTarget) focusTarget.focus({preventScroll: true});
      return;
    }
    if (name === "detail") { toggleDetail(trigger); return; }
    if (name === "prev" || name === "next") {
      const delta = name === "prev" ? -1 : 1;
      if (!menu.hidden) {
        menuIndex = wrap(menuIndex + delta);
        renderMenu();
        menuButtons[menuIndex].focus({preventScroll: true});
      } else {
        const focusWasInDetail = detail.contains(document.activeElement);
        selectScreen(index + delta);
        if (focusWasInDetail) root.focus({preventScroll: true});
      }
    }
  }

  tabs.forEach(tab => {
    tab.addEventListener("click", () => selectScreen(Number(tab.dataset.screen)));
    tab.addEventListener("keydown", event => {
      const target = {ArrowLeft: index - 1, ArrowRight: index + 1, Home: 0, End: definitions.length - 1}[event.key];
      if (target === undefined || event.altKey || event.ctrlKey || event.metaKey) return;
      event.preventDefault();
      if (!event.repeat) selectScreen(target, true);
    });
  });
  actionButtons.forEach(button => button.addEventListener("click", () => action(button.dataset.action, button)));
  menuButtons.forEach(button => button.addEventListener("click", () => {
    const focusTarget = returnFocus;
    selectScreen(Number(button.dataset.menu));
    if (focusTarget) focusTarget.focus({preventScroll: true});
  }));
  document.querySelectorAll("[data-close]").forEach(button => button.addEventListener("click", () => closeOverlay()));

  function handleKey(event) {
    if (event.altKey || event.ctrlKey || event.metaKey || event.target.closest("input, textarea, select, [contenteditable]")) return;
    if (event.key === "Escape" && (!menu.hidden || !detail.hidden)) { event.preventDefault(); closeOverlay(); return; }
    const codeActions = {Numpad4: "prev", Numpad6: "next", Numpad0: "menu", Numpad5: "detail", Digit4: "prev", Digit6: "next", Digit0: "menu", Digit5: "detail"};
    const keyActions = {ArrowLeft: "prev", ArrowRight: "next", Insert: "menu", Clear: "detail"};
    const name = codeActions[event.code] || keyActions[event.key];
    if (!name) return;
    event.preventDefault();
    if (!event.repeat) action(name, event.target);
  }
  root.addEventListener("keydown", handleKey);
  controls.addEventListener("keydown", handleKey);
  selectScreen(0);
})();
