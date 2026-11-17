// Track macros mockups: shared texts (the proposed catalog entries, de + en) and builders that
// mimic komponist's widgets. A mockup page calls `page(cfg)`; `#en` in the URL switches to English.

const LANG = location.hash === '#en' ? 'en' : 'de';
const T = {
  de: {
    macros: 'Spur-Makros', macro: n => `Spur-Makro ${n}`, unused: 'frei', map: 'Zuordnen',
    showInDevices: 'in Geräten zeigen', mappings: 'Zuordnungen', name: 'Name', value: 'Wert',
    targets: n => n === 1 ? '1 Ziel' : `${n} Ziele`, invert: 'Umkehren', remove: 'Zuordnung entfernen',
    range: (a, b) => `${a} bis ${b}`, missing: 'fehlt', pluginMissing: 'Plugin fehlt',
    paramMissing: 'Parameter fehlt', crashed: 'abgestürzt',
    pluginMissingHint: 'Die Zuordnung bleibt gespeichert und wirkt wieder, sobald das Plugin geladen ist.',
    paramMissingHint: 'Das Plugin hat diesen Parameter nicht mehr. Die Zuordnung bleibt gespeichert.',
    targetsMissing: n => n === 1 ? '1 Ziel fehlt' : `${n} Ziele fehlen`,
    controlledBy: 'Gesteuert von', drivenTip: (n, nm) => `Gesteuert von Spur-Makro ${n} „${nm}“`,
    showMacro: 'Spur-Makro zeigen', toDefault: 'Auf Standard', toDefaultKey: 'Doppelklick',
    enterValue: 'Wert eingeben', rename: 'Umbenennen', removeAll: 'Alle Zuordnungen entfernen', removeMenu: 'Zuordnung entfernen',
    bend: 'Kurve', bendTip: 'Kurve: ziehen biegt · Alt-Klick: linear', bendLinear: 'linear',
    mapHint: 'Parameter bewegen, um sie zuzuordnen', mapHint2: 'Esc beendet',
    mapStatus: n => `Zuordnen: Parameter bewegen ordnet sie Spur-Makro ${n} zu · Klick auf ein anderes Makro wechselt · Esc beendet`,
    lockedStatus: (p, n, nm) => `${p} wird von Spur-Makro ${n} „${nm}“ gesteuert`,
    mapChip: (n, nm) => `Zuordnen · Spur-Makro ${n} „${nm}“`, quote: nm => `„${nm}“`,
    noMappings: 'Keine Zuordnungen. „Zuordnen“ einschalten und Parameter bewegen.',
    // existing app texts
    arrangement: 'Arrangement', inspector: 'Inspector', track: 'Spur', devices: 'Geräte',
    channel: 'Kanal', parameter: 'Parameter', instrument: 'Instrument', input: 'Eingang',
    allInputs: 'Alle MIDI-Eingänge', midiCh: 'Kanal', omni: 'Omni', addFx: '+ Effekt',
    showInMixer: 'im Mixer zeigen', volume: 'Lautstärke', pan: 'Pan', grid: 'Raster', bar1: '1 Takt',
    tracks: '6 Spuren', noClip: 'Kein Clip', mixer: 'Mixer', addDevice: '+ Gerät', preset: 'Preset',
    dropHint: 'Effekt aus dem Browser hierher ziehen', newTrack: 'Neue Spur',
    newTrackHint: 'Instrument hierher ziehen oder Strg T', default: 'Standard', rangeL: 'Bereich',
    selected: 'Pad ausgewählt', intern: 'INTERN', statusRight: 'Kein Audio-Gerät · 48 kHz · 256 Samples · 5.3 ms',
    // device params
    type: n => `Typ ${n}`, freq: n => `Frequenz ${n}`, gain: n => `Gain ${n}`, q: n => `Q ${n}`,
    output: 'Ausgang', bandOn: n => `Band ${n} an`, analysis: 'Analyse', time: 'Zeit', feedback: 'Feedback',
    lowCut: 'Low Cut', highCut: 'High Cut', width: 'Breite', mix: 'Mix', noteValue: 'Notenwert',
    size: 'Größe', decay: 'Decay', predelay: 'Pre-Delay', damping: 'Dämpfung', diffusion: 'Diffusion',
    modul: 'Modul.', room: 'Raum', rooms: ['Zimmer', 'Halle', 'Kirche'], freeze: 'Einfrieren', wetOnly: 'Nur Hall',
    cutoff: 'Cutoff', resonance: 'Resonanz', drive: 'Drive', envAmount: 'Hk. Menge',
    bandTypes: ['LC', 'LS', 'Gl.', 'HS', 'HC'], pingpong: 'Ping-Pong', sync: 'Sync', chorusMix: 'Mix',
  },
  en: {
    macros: 'Track Macros', macro: n => `Track macro ${n}`, unused: 'unused', map: 'Map',
    showInDevices: 'show in devices', mappings: 'Mappings', name: 'Name', value: 'Value',
    targets: n => n === 1 ? '1 target' : `${n} targets`, invert: 'Invert', remove: 'Remove mapping',
    range: (a, b) => `${a} to ${b}`, missing: 'missing', pluginMissing: 'Plugin Missing',
    paramMissing: 'Parameter Missing', crashed: 'crashed',
    pluginMissingHint: 'The mapping stays saved and works again as soon as the plugin loads.',
    paramMissingHint: 'The plugin no longer has this parameter. The mapping stays saved.',
    targetsMissing: n => n === 1 ? '1 target missing' : `${n} targets missing`,
    controlledBy: 'Controlled by', drivenTip: (n, nm) => `Controlled by track macro ${n} “${nm}”`,
    showMacro: 'Show Track Macro', toDefault: 'Reset to Default', toDefaultKey: 'Double-click',
    enterValue: 'Enter Value', rename: 'Rename', removeAll: 'Remove All Mappings', removeMenu: 'Remove Mapping',
    bend: 'Curve', bendTip: 'Curve: drag to bend · Alt+click: linear', bendLinear: 'linear',
    mapHint: 'Move parameters to map them', mapHint2: 'Esc ends',
    mapStatus: n => `Map: move parameters to map them to track macro ${n} · Click another macro to switch · Esc ends`,
    lockedStatus: (p, n, nm) => `${p} is controlled by track macro ${n} “${nm}”`,
    mapChip: (n, nm) => `Map · Track macro ${n} “${nm}”`, quote: nm => `“${nm}”`,
    noMappings: 'No mappings. Turn on “Map” and move parameters.',
    arrangement: 'Arrangement', inspector: 'Inspector', track: 'Track', devices: 'Devices',
    channel: 'Channel', parameter: 'Parameter', instrument: 'Instrument', input: 'Input',
    allInputs: 'All MIDI Inputs', midiCh: 'Channel', omni: 'Omni', addFx: '+ Effect',
    showInMixer: 'show in mixer', volume: 'Volume', pan: 'Pan', grid: 'Grid', bar1: '1 Bar',
    tracks: '6 Tracks', noClip: 'No Clip', mixer: 'Mixer', addDevice: '+ Device', preset: 'Preset',
    dropHint: 'Drag an effect here from the browser', newTrack: 'New Track',
    newTrackHint: 'Drag an instrument here or press Ctrl+T', default: 'Default', rangeL: 'Range',
    selected: 'Pad selected', intern: 'BUILT-IN', statusRight: 'No Audio Device · 48 kHz · 256 Samples · 5.3 ms',
    type: n => `Type ${n}`, freq: n => `Frequency ${n}`, gain: n => `Gain ${n}`, q: n => `Q ${n}`,
    output: 'Output', bandOn: n => `Band ${n} On`, analysis: 'Analyze', time: 'Time', feedback: 'Feedback',
    lowCut: 'Low Cut', highCut: 'High Cut', width: 'Width', mix: 'Mix', noteValue: 'Note Value',
    size: 'Size', decay: 'Decay', predelay: 'Pre-Delay', damping: 'Damping', diffusion: 'Diffusion',
    modul: 'Mod.', room: 'Room', rooms: ['Room', 'Hall', 'Church'], freeze: 'Freeze', wetOnly: 'Wet Only',
    cutoff: 'Cutoff', resonance: 'Resonance', drive: 'Drive', envAmount: 'Env. Amt',
    bandTypes: ['LC', 'LS', 'Bell', 'HS', 'HC'], pingpong: 'Ping-Pong', sync: 'Sync', chorusMix: 'Mix',
  },
};
const t = T[LANG];
const pct = v => LANG === 'de' ? `${Math.round(v)} %` : `${Math.round(v)}%`;

// ---- Knob (`widget.knob`: svg 32, Ø 28, track r 12, stroke 2, cap r 9, indicator 1.5, sweep 270;
// `mod_ring`: r + 2.5, stroke 1.5, track colour).
function arc(cx, cy, r, a0, a1) {
  const p = a => [cx + r * Math.cos(a * Math.PI / 180), cy + r * Math.sin(a * Math.PI / 180)];
  const [x0, y0] = p(a0), [x1, y1] = p(a1);
  return `M${x0.toFixed(2)} ${y0.toFixed(2)} A${r} ${r} 0 ${a1 - a0 > 180 ? 1 : 0} 1 ${x1.toFixed(2)} ${y1.toFixed(2)}`;
}
function knob(v, o = {}) {
  const d = o.d || 28, s = d / 28, size = d + 4, c = size / 2, r = 12 * s, start = 135;
  const ang = start + 270 * v;
  const valColor = o.color || 'var(--t2)';
  let svg = `<svg width="${size}" height="${size}" viewBox="0 0 ${size} ${size}">`;
  svg += `<path d="${arc(c, c, r, start, start + 270)}" stroke="var(--s5)" stroke-width="${2 * s}" fill="none" stroke-linecap="round"/>`;
  if (v > 0.002) svg += `<path d="${arc(c, c, r, start, ang)}" stroke="${valColor}" stroke-width="${2 * s}" fill="none" stroke-linecap="round"/>`;
  if (o.mod) {
    const [lo, hi] = o.mod, rr = r + 2.5 * s;
    svg += `<path d="${arc(c, c, rr, start + 270 * lo, start + 270 * hi)}" stroke="var(--hue)" stroke-width="1.5" fill="none" stroke-linecap="round"/>`;
  }
  svg += `<circle cx="${c}" cy="${c}" r="${9 * s}" fill="var(--s3)"/>`;
  const a = ang * Math.PI / 180;
  svg += `<line x1="${c + 2.5 * s * Math.cos(a)}" y1="${c + 2.5 * s * Math.sin(a)}" x2="${c + 9 * s * Math.cos(a)}" y2="${c + 9 * s * Math.sin(a)}" stroke="${o.indicator || 'var(--t1)'}" stroke-width="1.5" stroke-linecap="round"/>`;
  return svg + '</svg>';
}

// A knob cell of the device grid at grid column/row (`col_x = 8 + col·60`, `row_y = 8 + row·68`).
function cell(col, row, o) {
  const x = 8 + col * 60, y = 8 + row * 68;
  const cls = ['cell', o.large ? 'large' : '', o.cls || ''].join(' ');
  const k = knob(o.v, { d: o.large ? 44 : 28, mod: o.mod, color: o.color, indicator: o.indicator });
  let b = '';
  if (o.badge) b = `<div class="badge ${o.badgeCls || 'drive'}" style="${o.large ? 'left:2px' : 'right:1px'};top:1px">${o.badge}</div>`;
  return `<div class="${cls}" style="left:${x}px;top:${y}px"><div class="kn">${k}</div>` +
    `<div class="nm" style="${o.nameStyle || ''}">${o.name}</div><div class="vl" style="${o.valStyle || ''}">${o.value}</div>${b}${o.extra || ''}</div>`;
}

// A device frame in the chain at x (relative to the chain), width w.
function frame(x, w, o) {
  return `<div class="frame ${o.cls || ''}" style="left:${x}px;width:${w}px">` +
    `<div class="head">${o.head}</div><div class="body">${o.body}</div>${o.over || ''}</div>`;
}
const W = cols => 16 + 60 * cols - 4;
// The mapping-mode ground behind a non-knob widget's grid area.
const area = (o, x, y, w, h) => o && o.cls ? `<div class="abs ${o.cls}" style="left:${x}px;top:${y}px;width:${w}px;height:${h}px;border-radius:3px"></div>` : '';

function head(name, o = {}) {
  let h = o.noPower ? '' : `<div class="power"></div>`;
  if (o.bar) h += `<div class="tbar"></div>`;
  h += `<div class="nm">${name}</div><div style="flex:1"></div>`;
  if (o.right) h += o.right;
  else h += `<div class="preset">${o.preset || t.preset}<span class="t2" style="font-size:8px">▼</span></div>`;
  h += `<div class="menu3">•••</div>`;
  return h;
}

// ---- The track macro card (4 columns, rows 0–1 knobs, row 2 info).
function macroCard(x, macros, o = {}) {
  let body = '';
  macros.forEach((m, i) => {
    const col = i % 4, row = Math.floor(i / 4);
    const named = m.name || (m.targets ? m.auto : '');
    const sel = o.selected === i + 1;
    let cls = '';
    if (sel && o.mapping) cls = 'learn'; else if (sel) cls = 'selring';
    const unusedName = `<span class="t3">${t.unused}</span>`;
    body += cell(col, row, {
      v: m.v, value: m.targets ? pct(m.v * 100) : `<span class="t3">${pct(m.v * 100)}</span>`,
      name: named ? named : unusedName, color: m.targets ? 'var(--hue)' : 'var(--t3)',
      indicator: m.targets ? 'var(--t1)' : 'var(--t3)',
      badge: String(i + 1), badgeCls: (sel ? 'sel' : 'n') + (m.warn ? ' warn' : ''), cls,
    });
  });
  // Row 2: the selected macro in words.
  const s = macros[(o.selected || 1) - 1];
  const sname = s.name || s.auto || t.unused;
  let info = `<div class="abs" style="left:8px;top:144px;width:236px;height:64px;border-top:1px solid var(--l1);padding-top:8px">`;
  if (o.mapping) {
    info += `<div style="font-size:11px;line-height:14px;color:var(--accent);font-weight:600">${t.mapHint}</div>` +
      `<div class="small" style="margin-top:3px">${t.macro(o.selected)} ${t.quote(sname)} · ${t.mapHint2}</div>`;
  } else {
    info += `<div style="display:flex;gap:6px;align-items:center"><div class="badge sel" style="position:static">${o.selected || 1}</div>` +
      `<span style="font-weight:600">${sname}</span><span class="small" style="margin-left:auto">${t.targets(s.targets || 0)}</span></div>` +
      `<div class="small" style="margin-top:5px;white-space:nowrap;overflow:hidden;text-overflow:ellipsis">${(s.list || []).join(' · ')}</div>`;
  }
  info += `</div>`;
  body += info;
  const right = `<div class="btn compact ${o.mapping ? 'on' : ''}">${t.map}</div>`;
  return frame(x, W(4), { cls: 'macros', head: head(t.macros, { noPower: true, bar: true, right }), body });
}

// A collapsed device strip (28 px).
function collapsed(x, name, o = {}) {
  return `<div class="frame" style="left:${x}px;width:28px;background:var(--s3)">` +
    `<div class="power" style="position:absolute;left:9px;top:9px"></div>` +
    `<div style="position:absolute;left:7px;top:30px;writing-mode:vertical-rl;font-weight:600;font-size:12px">${name}</div>${o.extra || ''}</div>`;
}

// ---- Built-in devices as in the app's snapshots.
function eqCard(x, o = {}) {
  const m = o.map || {};
  const c = k => m[k] || {};
  // Graph: the EQ curve with 5 handles, band 5 (high cut) selected.
  const gw = 356, gh = 132;
  const hcX = o.hcX || 300;
  const curve = `M0 ${gh - 10} C 30 ${gh - 40}, 50 70, 90 70 L 150 72 C 190 72, 200 60, 220 58 C 240 56, 255 70, ${hcX - 30} 72 C ${hcX - 10} 74, ${hcX + 10} 90, ${hcX + 30} ${gh}`;
  let g = `<div class="abs ${o.graphCls || ''}" style="left:8px;top:8px;width:${gw}px;height:${gh}px;background:var(--s1);border-radius:3px;overflow:hidden">` +
    `<svg width="${gw}" height="${gh}">`;
  for (const gx of [40, 92, 144, 196, 248, 300]) g += `<line x1="${gx}" y1="0" x2="${gx}" y2="${gh}" stroke="var(--l1)"/>`;
  for (const gy of [33, 66, 99]) g += `<line x1="0" y1="${gy}" x2="${gw}" y2="${gy}" stroke="${gy === 66 ? 'var(--l2)' : 'var(--l1)'}"/>`;
  g += `<path d="${curve} L ${gw} ${gh} L 0 ${gh} Z" fill="var(--hue)" fill-opacity="0.14"/>`;
  g += `<path d="${curve}" stroke="var(--hue)" stroke-width="1.5" fill="none"/>`;
  const hs = [[60, 92, 1], [118, 71, 2], [180, 66, 3], [228, 57, 4], [hcX, 76, 5]];
  for (const [hx, hy, n] of hs) {
    const sel = n === 5;
    g += `<circle cx="${hx}" cy="${hy}" r="5" fill="var(--s0)" stroke="${sel ? 'var(--accent)' : 'var(--t1)'}" stroke-width="${sel ? 2 : 1.5}"/>` +
      `<text x="${hx}" y="${hy + 3}" font-family="IBM Plex Mono" font-size="8" fill="var(--t1)" text-anchor="middle">${n}</text>`;
    if (sel && o.hcBadge) g += `<circle cx="${hx}" cy="${hy}" r="8" fill="none" stroke="var(--hue)" stroke-width="1.5"/>`;
  }
  g += `<text x="${gw - 22}" y="${gh - 4}" font-family="IBM Plex Mono" font-size="8" fill="var(--t2)">10k</text>`;
  g += `<text x="${92 + 3}" y="${gh - 4}" font-family="IBM Plex Mono" font-size="8" fill="var(--t2)">100</text>`;
  g += `<text x="${196 + 3}" y="${gh - 4}" font-family="IBM Plex Mono" font-size="8" fill="var(--t2)">1k</text>`;
  g += `</svg></div>`;
  let b = g + area(c('type'), 4, 141, 124, 33) + area(c('on'), 4, 182, 124, 28);
  b += `<div class="minilabel" style="left:8px;top:144px">${t.type(5)}</div>`;
  b += `<div class="seg" style="left:8px;top:157px;width:116px">${t.bandTypes.map((s, i) => `<span class="${i === 4 ? 'on' : ''}">${s}</span>`).join('')}</div>`;
  b += `<div class="tgl on" style="left:8px;top:186px;width:56px">${t.bandOn(5)}</div>`;
  b += `<div class="tgl on" style="left:68px;top:186px;width:56px">${t.analysis}</div>`;
  b += cell(2, 2, { v: o.freqV ?? 0.82, name: t.freq(5), value: o.freqText || '12.0 kHz', ...c('freq') });
  b += cell(3, 2, { v: 0.5, name: t.gain(5), value: '0.0 dB', ...c('gain') });
  b += cell(4, 2, { v: 0.3, name: t.q(5), value: '0.71', ...c('q') });
  b += cell(5, 2, { v: 0.5, name: t.output, value: '0.0 dB', ...c('out') });
  return frame(x, W(6), { head: head('EQ', { preset: 'Low Cut 80 Hz' }), body: b });
}

function delayCard(x, o = {}) {
  const m = o.map || {}; const c = k => m[k] || {};
  let b = `<div class="abs ${c('xy').cls || ''}" style="left:8px;top:8px;width:116px;height:132px;background:var(--s1);border-radius:3px">` +
    `<svg width="116" height="132"><line x1="0" y1="66" x2="116" y2="66" stroke="var(--l1)"/><line x1="80" y1="0" x2="80" y2="132" stroke="var(--l1)"/>` +
    `<circle cx="80" cy="66" r="5" fill="var(--hue)"/></svg>` +
    `<div class="abs" style="left:6px;top:4px;font-size:9px;line-height:12px;color:var(--t2)">${t.feedback} <span class="mono">42 %</span></div>` +
    `<div class="abs" style="right:6px;bottom:4px;font-size:9px;line-height:12px;color:var(--t2)">${t.time} <span class="mono">250 ms</span></div></div>`;
  b += cell(2, 0, { v: 0.45, name: t.time, value: '250 ms', ...c('time') });
  b += cell(3, 0, { v: 0.42, name: t.feedback, value: '42 %', ...c('fb') });
  b += cell(2, 1, { v: 0.05, name: t.lowCut, value: '20 Hz', ...c('lc') });
  b += cell(3, 1, { v: 1, name: t.highCut, value: '20 kHz', ...c('hc') });
  b += area(c('pp'), 4, 145, 124, 66);
  b += `<div class="seg" style="left:8px;top:152px;width:116px"><span class="on">${t.pingpong}</span><span>${t.sync}</span></div>`;
  b += `<div class="minilabel" style="left:8px;top:176px">${t.noteValue}</div>`;
  b += `<div class="abs" style="left:8px;top:189px;width:116px;height:18px;border-radius:3px;background:var(--s3);font-size:11px;line-height:18px;padding:0 6px">1/4</div>`;
  b += cell(2, 2, { v: 1, name: t.width, value: '100 %', ...c('width') });
  b += cell(3, 2, { v: o.mixV ?? 0.3, name: t.mix, value: o.mixText || '30 %', ...c('mix') });
  return frame(x, W(4), { head: head('Delay', { preset: 'Soft Tape' }), body: b });
}

function hallCard(x, o = {}) {
  const m = o.map || {}; const c = k => m[k] || {};
  let b = '';
  b += cell(0, 0, { large: true, v: o.sizeV ?? 0.55, name: t.size, value: o.sizeText || '28 m', ...c('size') });
  b += cell(2, 0, { v: o.decayV ?? 0.4, name: t.decay, value: o.decayText || '2.40 s', ...c('decay') });
  b += cell(3, 0, { v: 0.2, name: t.predelay, value: '18 ms', ...c('pre') });
  b += cell(0, 1, { v: 0.6, name: t.damping, value: '5.2 kHz', ...c('damp') });
  b += cell(1, 1, { v: 0.7, name: t.diffusion, value: '70 %', ...c('diff') });
  b += cell(2, 1, { v: 0.9, name: t.width, value: '90 %', ...c('width') });
  b += cell(3, 1, { v: 0.25, name: t.modul, value: '25 %', ...c('mod') });
  b += cell(0, 2, { large: true, v: o.mixV ?? 0.22, name: t.mix, value: o.mixText || '22 %', ...c('mix') });
  b += area(c('room'), 124, 141, 124, 33) + area(c('fr'), 124, 182, 124, 28);
  b += `<div class="minilabel" style="left:128px;top:144px">${t.room}</div>`;
  b += `<div class="seg" style="left:128px;top:157px;width:116px">${t.rooms.map((s, i) => `<span class="${i === 1 ? 'on' : ''}">${s}</span>`).join('')}</div>`;
  b += `<div class="tgl" style="left:128px;top:186px;width:56px">${t.freeze}</div>`;
  b += `<div class="tgl" style="left:188px;top:186px;width:56px">${t.wetOnly}</div>`;
  return frame(x, W(4), { head: head(LANG === 'de' ? 'Hall' : 'Reverb', { preset: 'Large Hall' }), body: b });
}

// ---- Shell: transport, collapsed browser, arrangement, dock with the device chain, inspector.
const TRACKS = [
  { name: 'Drums', sub: 'Sampler', c: '#dc855d', body: '#462312' },
  { name: 'Bass', sub: 'Volt', c: '#61a3e6', body: '#142f4b' },
  { name: 'Lead', sub: 'Hexa', c: '#63b376', body: '#14361d' },
  { name: 'Pad', sub: 'Volt', c: '#aa8dde', body: '#332647', sel: true },
  { name: 'Keys', sub: 'Hexa', c: '#b69d3a', body: '#372d02' },
  { name: 'Arp', sub: 'Hexa', c: '#00b4bc', body: '#00363a' },
];
const CLIPS = [
  [0, 1, 16], [1, 1, 8], [1, 9, 16], [2, 9, 16], [3, 1, 16], [4, 5, 8], [4, 13, 16], [5, 9, 16],
];
function arrangement() {
  const x0 = 24, top = 44;
  let h = `<div class="abs" style="left:${x0}px;top:${top}px;width:1616px;height:696px;background:var(--s0)">`;
  h += `<div class="panelhead abs" style="left:0;top:0;width:1616px"><span class="label" style="color:var(--t1)">${t.arrangement}</span>` +
    `<span style="margin-left:150px" class="small">${t.grid}</span><span style="font-size:11px">${t.bar1}</span></div>`;
  h += `<div class="abs" style="left:10px;top:34px;font:400 10px/12px var(--mono);color:var(--t3)">${t.tracks}</div>`;
  // ruler
  h += `<div class="abs" style="left:224px;top:28px;width:1392px;height:22px;border-bottom:1px solid var(--l1)">`;
  for (let b = 0; b < 20; b++) h += `<div class="abs mono" style="left:${b * 72 + 3}px;top:4px;font-size:10px;color:var(--t2)">${b + 1}</div>` +
    `<div class="abs" style="left:${b * 72}px;top:12px;width:1px;height:10px;background:var(--l2)"></div>`;
  h += `</div>`;
  for (let b = 0; b < 20; b++) h += `<div class="abs" style="left:${224 + b * 72}px;top:52px;width:1px;height:644px;background:var(--l0)"></div>`;
  TRACKS.forEach((tr, i) => {
    const y = 52 + i * 56;
    const bg = tr.sel ? 'background:#1c2a38' : '';
    h += `<div class="trackhead" style="left:0;top:${y}px;${bg}"><div class="bar" style="background:${tr.c}"></div>` +
      `<div class="name">${tr.name}</div><div class="sub">${tr.sub}</div>` +
      ['M', 'S', '●', '◖'].map((s, j) => `<div class="tb" style="left:${118 + j * 20}px">${s}</div>`).join('') + `</div>`;
    if (tr.sel) h += `<div class="abs" style="left:224px;top:${y}px;width:1392px;height:56px;background:var(--lane-selected)"></div>`;
  });
  for (const [ti, a, b] of CLIPS) {
    const tr = TRACKS[ti], y = 52 + ti * 56 + 2;
    const x = 224 + (a - 1) * 72 + 1, w = (b - a + 1) * 72 - 2;
    let notes = '';
    for (let n = 0; n < w / 18; n++) {
      const ny = 20 + ((n * 7 + ti * 3) % 5) * 6;
      notes += `<div class="abs" style="left:${n * 18 + 4}px;top:${ny}px;width:${ti === 3 ? 140 : 10}px;height:2px;background:${tr.c};opacity:.8"></div>`;
    }
    h += `<div class="clip" style="left:${x}px;top:${y}px;width:${w}px;height:52px;background:${tr.body}">` +
      `<div class="ch" style="background:${tr.c}">${tr.name}</div>${notes}</div>`;
  }
  h += `<div class="abs" style="left:4px;top:${52 + 6 * 56 + 6}px;width:216px;min-height:54px;border:1px dashed var(--l2);border-radius:3px;padding:8px 10px">` +
    `<div>${t.newTrack}</div><div class="small t3" style="margin-top:2px">${t.newTrackHint}</div></div>`;
  // playhead
  h += `<div class="abs" style="left:${224 + 8 * 72 + 30}px;top:28px;width:1px;height:668px;background:var(--t1)"></div>`;
  return h + `</div>`;
}

function inspectorTrack() {
  const sw = ['#df7f78', '#dc855d', '#ce9042', '#b69d3a', '#8dab54', '#63b376', '#30b69a', '#00b4bc', '#30add6', '#61a3e6', '#8998e9', '#aa8dde', '#c483c7', '#d67ea9'];
  return `<div class="isec"><div class="hd"><span class="label">${t.track}</span><span class="meta">4 / 6</span></div>` +
    `<div style="display:flex;align-items:center;gap:8px;height:20px"><div class="tbar" style="height:16px"></div><span style="font-size:13px;font-weight:600">Pad</span></div>` +
    `<div style="display:flex;gap:4px;margin:8px 0 6px 0">${sw.map(c => `<div style="width:14px;height:14px;border-radius:2px;background:${c};${c === '#aa8dde' ? 'box-shadow:0 0 0 1px var(--s1),0 0 0 2px var(--t1)' : ''}"></div>`).join('')}</div>` +
    `<div class="irow"><span class="k">${t.instrument}</span><div class="dd">Volt<span class="t2" style="font-size:8px">▼</span></div></div>` +
    `<div class="irow"><span class="k">${t.input}</span><div class="dd">${t.allInputs}<span class="t2" style="font-size:8px">▼</span></div></div></div>`;
}
function inspectorDevices() {
  const rows = [['Volt', t.intern], ['EQ', t.intern], ['Delay', t.intern], [LANG === 'de' ? 'Hall' : 'Reverb', t.intern]];
  return `<div class="isec"><div class="hd"><span class="label">${t.devices}</span><span class="meta" style="font-family:var(--sans);font-size:10px">${t.addFx}</span></div>` +
    rows.map(([n, f], i) => `<div class="devrow" style="${i === 0 ? 'background:var(--s2)' : ''}"><div class="power"></div><span style="${i === 0 ? 'font-weight:600' : ''}">${n}</span><span class="fmt">${f}</span></div>`).join('') + `</div>`;
}
// The inspector's "Spur-Makros" section: 8 small knobs (`size.knob-small` 20) in 2 rows of 4.
function inspectorMacros(macros, o = {}) {
  let h = `<div class="isec"><div class="hd"><span class="label">${t.macros}</span><span class="meta">${t.showInDevices}</span></div>`;
  h += `<div style="position:relative;height:104px">`;
  macros.forEach((m, i) => {
    const x = (i % 4) * 64, y = Math.floor(i / 4) * 52;
    const sel = o.selected === i + 1;
    const nm = m.name || m.auto || `<span class="t3">${t.unused}</span>`;
    h += `<div class="abs" style="left:${x}px;top:${y}px;width:62px;height:50px;border-radius:3px;${sel ? (o.mapping ? 'background:var(--accent-surface);box-shadow:inset 0 0 0 1px var(--accent)' : 'box-shadow:inset 0 0 0 1px var(--accent)') : ''}">` +
      `<div class="abs" style="left:19px;top:3px">${knob(m.v, { d: 20, color: m.targets ? 'var(--hue)' : 'var(--t3)', indicator: m.targets ? 'var(--t1)' : 'var(--t3)' })}</div>` +
      `<div class="badge ${sel ? 'sel' : ''} ${m.warn ? 'warn' : ''}" style="left:4px;top:3px">${i + 1}</div>` +
      `<div class="abs" style="left:0;right:0;top:27px;text-align:center;font-size:10px;line-height:12px;color:var(--t2);white-space:nowrap;overflow:hidden">${nm}</div>` +
      `<div class="abs mono" style="left:0;right:0;top:38px;text-align:center;font-size:10px;line-height:12px;color:${m.targets ? 'var(--t1)' : 'var(--t3)'}">${pct(m.v * 100)}</div></div>`;
  });
  h += `</div>`;
  if (o.detail) h += o.detail;
  return h + `</div>`;
}
// The selected macro's detail: name and value, then the mapping list.
function macroDetail(n, m, rows, o = {}) {
  let h = `<div style="margin-top:12px;display:flex;gap:8px">` +
    `<div style="flex:1"><div class="small" style="margin-bottom:4px">${t.name}</div><div class="field sans">${m.name || `<span class="t3">${t.unused}</span>`}</div></div>` +
    `<div style="width:76px"><div class="small" style="margin-bottom:4px">${t.value}</div><div class="field">${pct(m.v * 100)}</div></div></div>`;
  h += `<div style="display:flex;align-items:center;margin:14px 0 8px 0;height:20px"><span class="small">${t.mappings} · ${rows.length}</span>` +
    `<div class="btn compact ${o.mapping ? 'on' : ''}" style="margin-left:auto">${t.map}</div></div>`;
  if (!rows.length) h += `<div class="small t3" style="padding:8px 0">${t.noMappings}</div>`;
  for (const r of rows) h += mapRow(r);
  return h;
}
function icInvert() {
  return `<svg width="12" height="12" viewBox="0 0 12 12"><path d="M2 4h7l-2-2M10 8H3l2 2" stroke="currentColor" stroke-width="1.3" fill="none" stroke-linecap="round" stroke-linejoin="round"/></svg>`;
}
// The curve control of a mapping (28 × 18): macro 0 % → 100 % left to right, the target from min (bottom) to max
// (top), bent with the automation spec's shape y = (e^(k·t) − 1) / (e^k − 1), k = 6 · bend.
function bendFn(t, b) { const k = 6 * b; return Math.abs(k) < 1e-3 ? t : (Math.exp(k * t) - 1) / (Math.exp(k) - 1); }
function curveCtl(b, inv, on) {
  let d = '';
  for (let i = 0; i <= 20; i++) {
    const v = i / 20, y = bendFn(inv ? 1 - v : v, b);
    d += `${i ? 'L' : 'M'}${(3 + v * 22).toFixed(1)} ${(15 - y * 12).toFixed(1)} `;
  }
  return `<div class="curvectl ${on ? 'on' : ''}"><svg width="28" height="18"><path d="${d}" stroke="var(--hue)" stroke-width="1.5" fill="none" stroke-linecap="round"/></svg></div>`;
}
function icRemove() {
  return `<svg width="10" height="10" viewBox="0 0 10 10"><path d="M2 2l6 6M8 2l-6 6" stroke="currentColor" stroke-width="1.3" stroke-linecap="round"/></svg>`;
}
// One mapping: device · parameter, invert, remove; the range bar in the parameter's own scale
// (handles at min and max, the fill between, the current value as a mark); the values from macro
// 0 % to macro 100 % (an inverted mapping reads high to low).
function mapRow(r) {
  const bad = r.missing || r.paramMissing;
  let h = `<div class="maprow">` +
    `<div class="tl"><span class="dev ${r.missing ? 'err' : ''}">${r.dev} ·</span><span class="par ${bad ? 'err' : ''}">${r.par}</span>` +
    (r.status ? `<span class="mono err" style="font-size:10px">${r.status}</span>` : '') +
    `<div style="flex:1"></div>${curveCtl(r.bend || 0, r.inv, r.bendOn)}<div class="ic ${r.inv ? 'on' : ''}" title="${t.invert}">${icInvert()}</div><div class="ic" title="${t.remove}">${icRemove()}</div></div>`;
  const lo = r.min * 100, hi = r.max * 100;
  h += `<div class="range ${bad ? 'dim' : ''}"><div class="trk"></div><div class="fill" style="left:${lo}%;width:${hi - lo}%"></div>` +
    (r.cur != null && !bad ? `<div class="cur" style="left:${r.cur * 100}%"></div>` : '') +
    `<div class="h" style="left:${lo}%"></div><div class="h" style="left:${hi}%"></div></div>`;
  if (bad) h += `<div class="small t3" style="font-size:10px;line-height:13px;margin-top:2px">${r.missing ? t.pluginMissingHint : t.paramMissingHint}</div>`;
  else h += `<div class="vals"><span>${r.inv ? t.range(r.hiText, r.loText) : t.range(r.loText, r.hiText)}</span><span style="color:var(--hue)">${r.curText || ''}</span></div>`;
  return h + `</div>`;
}

function page(cfg) {
  document.documentElement.lang = LANG;
  let h = '';
  // Transport
  h += `<div class="abs transport"><div class="abs" style="left:12px;top:14px;font-weight:600;font-size:13px">komponist <span style="font-weight:400;font-size:12px;color:var(--t2);margin-left:6px">Night Drive</span></div>` +
    `<div class="abs" style="left:264px;top:8px;display:flex;gap:6px">${['▶', '■', '●'].map((s, i) => `<div class="btn" style="width:28px;height:28px;${i === 2 ? 'color:var(--rec)' : ''}">${s}</div>`).join('')}</div>` +
    `<div class="abs mono" style="left:498px;top:10px;font-size:20px;line-height:24px;font-weight:500">9.1.000</div>` +
    `<div class="abs mono" style="left:592px;top:16px;font-size:11px;color:var(--t2)">00:15.483</div>` +
    `<div class="abs mono" style="left:734px;top:10px;height:24px;padding:0 8px;background:var(--s2);border-radius:3px;line-height:24px">124.00 <span class="t3" style="font-size:9px">BPM</span></div>` +
    `<div class="abs mono" style="left:840px;top:10px;height:24px;padding:0 8px;background:var(--s2);border-radius:3px;line-height:24px">4/4</div></div>`;
  // Collapsed browser strip, arrangement
  h += `<div class="abs" style="left:0;top:44px;width:24px;height:1014px;background:var(--s1);border-right:1px solid var(--l0)"><div style="color:var(--t2);font-size:8px;padding:9px 0 0 8px">▶</div></div>`;
  h += arrangement();
  // Dock
  const dx = 24, dy = 740, dw = 1616;
  h += `<div class="abs" style="left:${dx}px;top:${dy}px;width:${dw}px;height:318px;background:var(--s1);border-top:1px solid var(--l1)">`;
  h += `<div class="abs" style="left:0;top:0;height:28px;display:flex"><div class="tab">${t.noClip}</div><div class="tab on">${t.devices}</div><div class="tab">${t.mixer}</div><div class="tab t3">+</div></div>`;
  h += `<div class="abs" style="left:0;top:28px;width:${dw}px;height:28px;display:flex;align-items:center;gap:8px;padding:0 8px;background:var(--s1)">` +
    `<div style="width:8px;height:8px;border-radius:2px;background:var(--hue)"></div><span style="font-weight:600;margin-right:2px">Pad</span>` +
    ['Volt', 'EQ', 'Delay', LANG === 'de' ? 'Hall' : 'Reverb'].map(s => `<span class="chip">${s}</span>`).join('<span class="t3">›</span>') +
    (cfg.chainChip ? `<span class="chip" style="margin-left:16px;background:var(--accent-surface);color:var(--accent);box-shadow:inset 0 0 0 1px var(--accent)">${cfg.chainChip}</span>` : '') +
    `<div style="flex:1"></div><div class="btn compact">${t.addDevice}</div></div>`;
  h += `<div class="abs" style="left:10px;top:66px;width:${dw - 20}px;height:244px">${cfg.chain}</div>`;
  h += `</div>`;
  // Inspector
  h += `<div class="abs insp"><div class="panelhead" style="border-bottom:1px solid var(--l0)"><span class="t2" style="font-size:8px">▼</span><span class="label" style="color:var(--t1)">${t.inspector}</span></div>${cfg.inspector}</div>`;
  // Status bar
  h += `<div class="abs statusbar"><span style="color:var(--t1)">${cfg.statusLeft || t.selected}</span><span>${cfg.status || ''}</span>` +
    `<span style="margin-left:auto;font-family:var(--mono);font-size:10px;color:var(--t3)">${t.statusRight}</span></div>`;
  h += cfg.overlay || '';
  document.body.innerHTML = enPercent(h);
}
// English writes "40%" (docs/I18N.md); the device values above are typed the German way.
function enPercent(h) { return LANG === 'en' ? h.replace(/(\d) %/g, '$1%') : h; }
// A component sheet: components on surface.0 with small captions.
function sheet(h) { document.documentElement.lang = LANG; document.body.innerHTML = enPercent(h); }
const caption = (x, y, s) => `<div class="abs" style="left:${x}px;top:${y}px;max-width:290px;font-size:10px;line-height:12px;color:var(--t3)">${s}</div>`;

// The Pad track's macros in the mockups (names are content, like the demo's: English everywhere).
function padMacros() {
  return [
    { name: 'Open', v: 0.4, targets: 4, list: ['Cutoff', t.freq(5), t.mix, t.resonance] },
    { name: 'Space', v: 0.25, targets: 3, list: [t.mix, t.decay, t.size] },
    { name: 'Drive', v: 0.1, targets: 1, list: ['Drive'] },
    { name: '', auto: 'Cutoff', v: 0.0, targets: 0 },
    { name: '', v: 0, targets: 0 }, { name: '', v: 0, targets: 0 }, { name: '', v: 0, targets: 0 }, { name: '', v: 0, targets: 0 },
  ].map((m, i) => (i === 3 ? { name: '', v: 0, targets: 0 } : m));
}
