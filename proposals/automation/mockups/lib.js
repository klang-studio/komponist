// Helpers for the automation mockups. Geometry of the arrangement (1x points) from the app's snapshots:
// header column 260..484, bar 1 at x = 485, 72 px per bar (zoom of the S01 snapshot), track rows 56 px.
let BAR0 = 485, PX_PER_BAR = 72, BODY_X0 = 485, BODY_X1 = 1638;
const X = (bar) => BAR0 + (bar - 1) * PX_PER_BAR;
const SVGNS = "http://www.w3.org/2000/svg";
const ICON = {
  spline: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2.2" stroke-linecap="round" stroke-linejoin="round"><circle cx="19" cy="5" r="2"/><circle cx="5" cy="19" r="2"/><path d="M5 17A12 12 0 0 1 17 5"/></svg>',
  x: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><path d="M18 6 6 18M6 6l12 12"/></svg>',
  search: '<svg viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round"><circle cx="11" cy="11" r="7"/><path d="m20 20-3.5-3.5"/></svg>',
};

function el(tag, cls, style, html) {
  const e = document.createElement(tag);
  if (cls) e.className = cls;
  if (style) Object.assign(e.style, style);
  if (html !== undefined) e.innerHTML = html;
  document.body.appendChild(e);
  return e;
}
const px = (n) => n + "px";

// A crop of a snapshot (source rect sx,sy,w,h in 1x points) placed at dx,dy.
function crop(sx, sy, w, h, dx, dy, extraCls) {
  return el("div", "snap" + (extraCls ? " " + extraCls : ""), {
    left: px(dx), top: px(dy), width: px(w), height: px(h),
    backgroundPosition: `${-sx}px ${-sy}px`,
  });
}
function rect(x, y, w, h, color, extra) {
  return el("div", "fill", Object.assign({ left: px(x), top: px(y), width: px(w), height: px(h), background: color }, extra || {}));
}
function text(x, y, cls, html, style) {
  return el("div", "abs " + cls, Object.assign({ left: px(x), top: px(y), whiteSpace: "nowrap" }, style || {}), html);
}

// Track header. srcY: the snapshot row whose toggles and meter are reused (x 376..484).
// auto: "none" | "hidden" | "shown" | "rec"
function trackHeader(o) {
  const h = el("div", "thead" + (o.sel ? " sel" : ""), { top: px(o.y), height: px(55) });
  const bar = document.createElement("div"); bar.className = "bar"; bar.style.background = o.color; h.appendChild(bar);
  const n = document.createElement("div"); n.className = "name t-track"; n.textContent = o.name; h.appendChild(n);
  if (o.muted) n.style.color = "var(--text-3)";
  const s = document.createElement("div"); s.className = "sub t-small" + (o.subErr ? " err" : ""); s.textContent = o.sub; h.appendChild(s);
  if (!o.subErr) s.style.color = "var(--text-3)";
  // toggles M S, record, monitor, meter: reused pixels of the snapshot row
  crop(376, o.srcY + 1, 108, 54, 376, o.y);
  // the new automation toggle, its own group left of M S (gap 4 + 2)
  const t = document.createElement("div");
  t.className = "toggle" + (o.auto === "shown" ? " on" : "") + (o.auto === "rec" ? " rec" : "");
  t.style.left = px(354 - 260); t.style.top = px(20);
  t.innerHTML = ICON.spline;
  if (o.auto === "hidden") t.style.color = o.color;
  if (o.auto === "none") t.style.color = "var(--text-3)";
  h.appendChild(t);
  return h;
}

// Lane header. o: {y,h,color,dev,param,mode,value,warn,sel,writing,chip:"read"|"off"|"write"}
function laneHeader(o) {
  const h = el("div", "lhead" + (o.sel ? " sel" : ""), { top: px(o.y), height: px(o.h) });
  h.innerHTML = `<div class="bar" style="background:${o.color}"></div><div class="tree"></div>`;
  const c = document.createElement("div");
  c.className = "chooser t-ui" + (o.open ? " open" : "") + (o.hover ? " hover" : "");
  if (o.empty) c.innerHTML = `<span style="color:var(--text-2)">${o.empty}</span><span class="caret"></span>`;
  else c.innerHTML = (o.dev ? `<span class="dev">${o.dev}&nbsp;·&nbsp;</span>` : "") +
    `<span style="color:${o.paramColor || "var(--text-1)"}">${o.param}</span><span class="caret"></span>`;
  h.appendChild(c);
  const chip = document.createElement("div");
  chip.className = "chip" + (o.chip === "write" ? " write" : "") + (o.chip === "off" ? " off" : "");
  chip.innerHTML = `${o.mode}<span class="caret"></span>`;
  h.appendChild(chip);
  if (o.h >= 40) {
    if (o.warn) {
      const w = document.createElement("div"); w.className = "warn"; w.textContent = o.warn; h.appendChild(w);
    } else if (o.value) {
      const v = document.createElement("div"); v.className = "val t-vs";
      v.style.color = o.writing ? "var(--record-ink)" : "var(--text-2)"; v.textContent = o.value; h.appendChild(v);
    }
    if (o.writing) {
      const b = document.createElement("div"); b.className = "badge"; b.innerHTML = `<i></i>${o.writing}`; h.appendChild(b);
    }
  }
  return h;
}

// --- curves ---------------------------------------------------------------------------------------
// points: [{b: bar (1-based, fractional), v: 0..1, s: "lin" | "hold" | tension (number)}]
function shapeAt(s, t) {
  if (s === "hold") return 0;
  if (typeof s === "number" && s !== 0) { const k = 6 * s; return (Math.exp(k * t) - 1) / (Math.exp(k) - 1); }
  return t;
}
function valueAt(points, b) {
  if (b <= points[0].b) return points[0].v;
  for (let i = 0; i < points.length - 1; i++) {
    const p = points[i], q = points[i + 1];
    if (b >= p.b && b < q.b) return p.v + (q.v - p.v) * shapeAt(p.s, (b - p.b) / (q.b - p.b));
  }
  return points[points.length - 1].v;
}
// polyline in screen coords for a lane body
function curvePath(points, y, h, xFrom, xTo, pad) {
  pad = pad === undefined ? 5 : pad;
  const Y = (v) => y + pad + (1 - v) * (h - 2 * pad);
  const out = [];
  for (let x = xFrom; x <= xTo; x += 1) {
    const b = 1 + (x - BAR0) / PX_PER_BAR;
    out.push([x, Y(valueAt(points, b))]);
  }
  // exact verticals for holds
  return out;
}
function svgLayer(x, y, w, h) {
  const s = document.createElementNS(SVGNS, "svg");
  s.setAttribute("width", w); s.setAttribute("height", h);
  s.setAttribute("viewBox", `${x} ${y} ${w} ${h}`);
  Object.assign(s.style, { position: "absolute", left: px(x), top: px(y), overflow: "visible" });
  document.body.appendChild(s);
  return s;
}
function svgEl(svg, tag, attrs) {
  const e = document.createElementNS(SVGNS, tag);
  for (const k in attrs) e.setAttribute(k, attrs[k]);
  svg.appendChild(e);
  return e;
}

// Lane body. o: {y,h,points,line,fill,from,to,dash,width,pointsShown,selected:[i],pointColor,handles:[i],clipSpans:[{x0,x1,points}],bg}
function laneBody(o) {
  const y = o.y, h = o.h, pad = o.pad === undefined ? 5 : o.pad;
  const Y = (v) => y + pad + (1 - v) * (h - 2 * pad);
  if (o.bg !== false) rect(BODY_X0, y, BODY_X1 - BODY_X0, h, o.bg || "var(--surface-1)", { borderTop: "1px solid var(--line-0)" });
  const svg = svgLayer(BODY_X0, y, BODY_X1 - BODY_X0, h);
  // grid: bar lines line.2, beats none (as the arrangement at this zoom)
  for (let b = 2; X(b) < BODY_X1; b++) svgEl(svg, "line", { x1: X(b) + 0.5, y1: y + 1, x2: X(b) + 0.5, y2: y + h, stroke: "#45484d", "stroke-width": 1 });
  // 50 % guide
  svgEl(svg, "line", { x1: BODY_X0, y1: Math.round(Y(0.5)) + 0.5, x2: BODY_X1, y2: Math.round(Y(0.5)) + 0.5, stroke: "#2b2d30", "stroke-width": 1, "stroke-dasharray": "2 3" });
  return { svg, Y };
}
function drawCurve(svg, Y, points, o) {
  const from = o.from || BODY_X0, to = o.to || BODY_X1;
  const pts = [];
  for (let x = from; x <= to; x += 1) pts.push([x, Y(valueAt(points, 1 + (x - BAR0) / PX_PER_BAR))]);
  // insert exact hold jumps
  const d = pts.map((p, i) => (i ? "L" : "M") + p[0] + " " + p[1].toFixed(2)).join(" ");
  if (o.fill) {
    svgEl(svg, "path", { d: d + ` L ${to} ${o.bottom} L ${from} ${o.bottom} Z`, fill: o.fill, "fill-opacity": o.fillOpacity || 0.55, stroke: "none" });
  }
  svgEl(svg, "path", { d, fill: "none", stroke: o.line, "stroke-width": o.width || 1.5, "stroke-dasharray": o.dash || "", "stroke-linejoin": "round", opacity: o.opacity || 1 });
}
function drawPoints(svg, Y, points, o) {
  points.forEach((p, i) => {
    const cx = X(p.b), cy = Y(p.v);
    if (cx < (o.from || 0) || cx > (o.to || 9999)) return;
    const sel = (o.selected || []).includes(i);
    if (sel) svgEl(svg, "circle", { cx, cy, r: 4.5, fill: "none", stroke: "#4ea7e6", "stroke-width": 1 });
    svgEl(svg, "circle", { cx, cy, r: 3.5, fill: sel ? "#e8e9eb" : o.color, stroke: "#111214", "stroke-width": 1 });
  });
  (o.handles || []).forEach((i) => {
    const p = points[i], q = points[i + 1];
    const bm = (p.b + q.b) / 2, cx = X(bm), cy = Y(valueAt(points, bm));
    svgEl(svg, "path", { d: `M${cx} ${cy - 3.5} L${cx + 3.5} ${cy} L${cx} ${cy + 3.5} L${cx - 3.5} ${cy} Z`, fill: "#17181a", stroke: o.handleColor || "#e8e9eb", "stroke-width": 1 });
  });
}
// hatched span (clip override)
function hatch(svg, x0, x1, y, h, id) {
  const defs = svgEl(svg, "defs", {});
  const pat = document.createElementNS(SVGNS, "pattern");
  pat.setAttribute("id", id); pat.setAttribute("width", 6); pat.setAttribute("height", 6);
  pat.setAttribute("patternUnits", "userSpaceOnUse"); pat.setAttribute("patternTransform", "rotate(45)");
  const l = document.createElementNS(SVGNS, "line");
  l.setAttribute("x1", 0); l.setAttribute("y1", 0); l.setAttribute("x2", 0); l.setAttribute("y2", 6);
  l.setAttribute("stroke", "#111214"); l.setAttribute("stroke-width", 3); l.setAttribute("stroke-opacity", 0.6);
  pat.appendChild(l); defs.appendChild(pat);
  svgEl(svg, "rect", { x: x0, y: y + 1, width: x1 - x0, height: h - 1, fill: `url(#${id})` });
  svgEl(svg, "line", { x1: x0 + 0.5, y1: y + 1, x2: x0 + 0.5, y2: y + h, stroke: "#45484d" });
  svgEl(svg, "line", { x1: x1 - 0.5, y1: y + 1, x2: x1 - 0.5, y2: y + h, stroke: "#45484d" });
}
function playhead(x, y0, y1) { rect(x, y0, 1, y1 - y0, "var(--text-1)"); }

// English check rendering (rule 7): the new texts in English; the snapshot panels around them stay German.
const EN = [
  ["durch Clip-Automation", "by clip automation"], ["Parameter wählen …", "Choose Parameter…"],
  ["Parameter suchen", "Search parameters"], ["Zuletzt bewegt", "Recently Moved"], ["Spur-Makros", "Track Macros"],
  ["über Spur-Makro 1 „Open“", "via track macro 1 “Open”"], ["Lautstärke", "Volume"], ["Algorithmus", "Algorithm"],
  ["⚠ Plugin fehlt: Surge XT", "⚠ Plugin missing: Surge XT"], ["⚠ Parameter fehlt", "⚠ Parameter missing"],
  ["⚠ nicht automatisierbar", "⚠ not automatable"], ["⚠ Gerät nicht auf dieser Spur", "⚠ Device not on this track"],
  ["schreibt", "writing"], ["Lesen", "Read"], ["Aus", "Off"],
  ["Automation spielt nicht, Regler gelten", "Automation does not play; controls apply"],
  ["Spielt die Automation ab", "Plays the automation"], ["Schreibt, solange du einen Regler hältst", "Writes while you hold a control"],
  ["Schreibt ab dem ersten Griff bis zum Stopp", "Writes from the first touch until stop"],
  ["Form", "Shape"], ["Wert eingeben", "Enter Value"], ["Punkte löschen", "Delete Points"], ["Entf", "Del"],
  ["Kurve", "Curve"], ["Automation ausblenden", "Hide Automation"], ["Automationsspur hinzufügen", "Add Automation Lane"], ["Modus für alle Automationsspuren", "Mode for All Lanes"], ["Master", "Master"], ["Master · 2 Effekte", "Master · 2 effects"], ["Master · Lautstärke · 1 Punkt ausgewählt", "Master · Volume · 1 point selected"], ["Limiter · Eingang", "Limiter · Input"], ["Eingang", "Input"], ["Halten", "Hold"], ["Doppelklick setzt einen Punkt", "Double-click adds a point"],
  ["Doppelklick: Punkt · Strg-Ziehen: zeichnen · Alt-Ziehen: Kopie · Shift: fein", "Double-click: point · Ctrl+drag: draw · Alt+drag: copy · Shift: fine"],
  ["Lead · Feedback · 2 Punkte ausgewählt", "Lead · Feedback · 2 points selected"],
  ["Lead A · Clip-Automation Feedback · 1 Punkt ausgewählt", "Lead A · Clip automation Feedback · 1 point selected"],
  ["Lead · Automationsspur", "Lead · Automation lane"], ["Pfeiltasten wählen · Enter übernimmt · Esc schließt", "Arrows choose · Enter takes · Esc closes"],
  ["Lead · schreibt Feedback (Touch)", "Lead · writing Feedback (Touch)"],
  ["Loslassen: zurück zur Kurve in 250 ms · Esc verwirft den Durchgang", "Release: back to the curve in 250 ms · Esc discards the pass"],
  ["14 Punkte · Touch", "14 points · Touch"], ["Automation", "Automation"],
];
function translateEn() {
  if (!location.hash.includes("en")) return;
  const map = new Map(EN);
  const walk = document.createTreeWalker(document.body, NodeFilter.SHOW_TEXT);
  const nodes = []; while (walk.nextNode()) nodes.push(walk.currentNode);
  for (const n of nodes) {
    const t = n.nodeValue.replace(/ /g, " ").trim();
    if (map.has(t)) n.nodeValue = map.get(t);
  }
  document.documentElement.lang = "en";
}
