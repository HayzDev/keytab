// Keycap icons, shared by the switcher (index.html) and settings.html.
//
// Keys with a symbol (Enter, Shift, Tab, Backspace, Delete, \, arrows) are drawn as line
// icons on one 24px grid with round caps and joins, so every keycap has the same stroke weight
// and soft corners; Unicode glyphs come from whatever font has them and look jagged and uneven.
// Ctrl, esc and letters stay text.
(() => {
  window.keytabIcon = (name, size = 15, cls = '') =>
    `<span class="keytab-icon ${cls}" style="--icon: url(icons/${name}.svg); --size: ${size}px" aria-hidden="true"></span>`;
  const NAMES = {
    '↵': 'enter', Enter: 'enter', '⇧': 'shift', Shift: 'shift', '⇥': 'tab', Tab: 'tab',
    '⌫': 'backspace', Backspace: 'backspace', '⌦': 'delete', Del: 'delete', Delete: 'delete',
    '\\': 'backslash', '⧵': 'backslash', '↑': 'up', '↓': 'down',
  };
  const text = t => t.replace(/[&<>"]/g, c => `&#${c.charCodeAt(0)};`);

  const keyIcon = name => window.keytabIcon(name, 13, 'ki');

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
