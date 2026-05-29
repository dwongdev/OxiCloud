// Apply saved colour-scheme choice immediately (render-blocking) to prevent FOUC.
// This file MUST be loaded WITHOUT "defer" or "async" so it runs before any paint.
//
// localStorage values:
//   'light' / 'dark' → force that mode (stamp html[data-color-scheme]).
//   'auto'  / absent → follow the OS preference (no attribute → CSS default
//                      `color-scheme: light dark` resolves via prefers-color-scheme).
//
// Backward compat: existing users have 'light' / 'dark' stored from the old
// binary toggle; both still apply correctly. New 'auto' value is introduced
// by the segmented control in user-menu.

(() => {
    var saved = localStorage.getItem('oxicloud_theme');
    var html = document.documentElement;
    if (saved === 'light' || saved === 'dark') {
        html.setAttribute('data-color-scheme', saved);
    } else {
        html.removeAttribute('data-color-scheme');
    }
})();
