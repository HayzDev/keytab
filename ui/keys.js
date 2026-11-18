// Keycap icons, shared by the switcher (index.html) and settings.html.
//
// Keys with a symbol (Enter, Shift, Tab, Backspace, Delete, \, arrows) are drawn as line
// icons on one 24px grid with round caps and joins, so every keycap has the same stroke weight
// and soft corners; Unicode glyphs come from whatever font has them and look jagged and uneven.
// Ctrl, esc and letters stay text.
(() => {
  const PATHS = {
    enter: '<path d="M19 5v6.5a3 3 0 0 1-3 3H6"/><path d="m9.5 10.5-4 4 4 4"/>',
    shift: '<path d="M9 19.5v-5.5H5.6a.6.6 0 0 1-.43-1.02l6.4-6.6a.6.6 0 0 1 .86 0l6.4 6.6a.6.6 0 0 1-.43 1.02H15v5.5a.9.9 0 0 1-.9.9h-4.2a.9.9 0 0 1-.9-.9z"/>',
    tab: '<path d="M4 12h12.5"/><path d="m12.5 7.5 4.5 4.5-4.5 4.5"/><path d="M20.5 6.5v11"/>',
    backspace: '<path d="M9.6 5.5h9.4a2 2 0 0 1 2 2v9a2 2 0 0 1-2 2H9.6a2 2 0 0 1-1.5-.68L3.6 12.66a1 1 0 0 1 0-1.32l4.5-5.16a2 2 0 0 1 1.5-.68z"/><path d="m11.5 9.5 5 5m0-5-5 5"/>',
    delete: '<path d="M14.4 5.5H5a2 2 0 0 0-2 2v9a2 2 0 0 0 2 2h9.4a2 2 0 0 0 1.5-.68l4.5-5.16a1 1 0 0 0 0-1.32l-4.5-5.16a2 2 0 0 0-1.5-.68z"/><path d="m7.5 9.5 5 5m0-5-5 5"/>',
    backslash: '<path d="m7 4 10 16"/>',
    up: '<path d="M12 19V5.5"/><path d="m6 11 6-6 6 6"/>',
    down: '<path d="M12 5v13.5"/><path d="m6 13 6 6 6-6"/>',
  };
  const NAMES = {
    '↵': 'enter', Enter: 'enter', '⇧': 'shift', Shift: 'shift', '⇥': 'tab', Tab: 'tab',
    '⌫': 'backspace', Backspace: 'backspace', '⌦': 'delete', Del: 'delete', Delete: 'delete',
    '\\': 'backslash', '⧵': 'backslash', '↑': 'up', '↓': 'down',
  };
  const text = t => t.replace(/[&<>"]/g, c => `&#${c.charCodeAt(0)};`);

  const keyIcon = name => `<svg class="ki" viewBox="0 0 24 24" width="13" height="13" fill="none" stroke="currentColor"`
    + ` stroke-width="2.3" stroke-linecap="round" stroke-linejoin="round" aria-hidden="true">${PATHS[name]}</svg>`;

  // The inside of one keycap: an icon, "L"/"R" + the Shift icon, or the key's text.
  window.keycap = label => {
    const side = label.match(/^([LR])⇧$/);
    if (side) return `<span class="ks">${side[1]}</span>${keyIcon('shift')}`;
    // Esc's symbol reads as an undo arrow at keycap size, so it's the word, as in Raycast.
    if (label === '⎋' || label === 'Esc') return 'esc';
    const name = NAMES[label];
    return name ? keyIcon(name) : text(label);
  };
})();
