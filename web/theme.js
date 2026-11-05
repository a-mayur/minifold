'use strict';
// Loaded in <head> right after /theme.css, before the page renders, so dark mode never flashes.
// Mode: the visitor's own choice (remembered in this browser), else the admin's default.
(() => {
  const root = document.documentElement;
  const KEY = 'minifold-mode';
  const stored = () => {
    try {
      return localStorage.getItem(KEY);
    } catch {
      return null; // storage blocked (private window): fall back to the default each visit
    }
  };
  const adminDefault = () => {
    const mode = getComputedStyle(root).getPropertyValue('--default-mode').trim();
    if (mode === 'light' || mode === 'dark') return mode;
    return matchMedia('(prefers-color-scheme: dark)').matches ? 'dark' : 'light';
  };
  root.dataset.mode = stored() || adminDefault();

  function syncButton() {
    const button = document.getElementById('mode-toggle');
    if (!button) return;
    const dark = root.dataset.mode === 'dark';
    button.setAttribute('aria-label', dark ? 'Switch to light mode' : 'Switch to dark mode');
    button.title = button.getAttribute('aria-label');
    button.replaceChildren(icon(dark ? 'light_mode' : 'dark_mode'));
  }

  window.minifoldTheme = {
    /** Re-fetches /theme.css after the admin changed the theme. */
    reload() {
      const link = document.getElementById('theme-css');
      link.addEventListener('load', () => {
        if (!stored()) root.dataset.mode = adminDefault();
        syncButton();
      }, { once: true });
      link.href = `/theme.css?v=${Date.now()}`;
    },
  };

  document.addEventListener('DOMContentLoaded', () => {
    syncButton();
    const button = document.getElementById('mode-toggle');
    if (!button) return;
    button.addEventListener('click', () => {
      root.dataset.mode = root.dataset.mode === 'dark' ? 'light' : 'dark';
      try {
        localStorage.setItem(KEY, root.dataset.mode);
      } catch {
        // not remembered; still switched for this page
      }
      syncButton();
    });
  });
})();
