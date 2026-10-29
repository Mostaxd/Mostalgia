// Episode 9 · Themes: the mock window re-skins itself with the app's real
// themes (colours, corners, borders, fonts and the little scene from
// crates/mostalgia-app/assets/themes.json). On first view it spins through a
// few themes, drags the text size, flips the language and edits an accent
// into a theme of its own; then the controls are the visitor's.
(() => {
  const S = window.MostalgiaSite;
  const scene = document.getElementById('t9Scene');
  if (!S || !scene) return;
  const { reduce, $, $$ } = S;

  // id, English name, German name, background, foreground, surface, sidebar, border, muted,
  // muted foreground, primary, primary foreground, first three speakers, radius, border, font, size, scene
  const RAW = [
    ['mostalgia', 'Mostalgia', 'Mostalgia', '#FBF5EA', '#2B1730', '#FFFAF1', '#F3E6D0', '#E6D3B6', '#F1E3CC', '#6E4F5F', '#B8472A', '#FFF8EE', ['#B8472A', '#5B2A4E', '#2F6F73'], 10, 1, '', 14, 'steam'],
    ['mostalgia-nacht', 'Mostalgia Night', 'Mostalgia Nacht', '#1F1123', '#FBF3E4', '#2B1730', '#190D1C', '#4A2B44', '#33203A', '#C9B3BF', '#F2A541', '#2B1730', ['#F2A541', '#E58A6B', '#9DB8A0'], 10, 1, '', 14, 'steam'],
    ['sakura', 'Sakura', 'Sakura', '#fff7fa', '#3d2433', '#ffffff', '#fdeaf1', '#f4cfdd', '#fbe3ec', '#8f6577', '#bf3569', '#ffffff', ['#bf3569', '#6b8e23', '#7a5cc7'], 14, 1, '', 14, 'petals'],
    ['synthwave', 'Synthwave', 'Synthwave', '#160d2e', '#f9e8ff', '#1e1240', '#110a24', '#3d2670', '#28185a', '#b39ddb', '#ff3ea5', '#1a0010', ['#ff3ea5', '#2de2e6', '#ffd319'], 10, 1, '', 14, 'grid'],
    ['blockwelt', 'Blockworld', 'Blockwelt', '#3a3a3a', '#f4f4f4', '#4a4a4a', '#4b3621', '#141414', '#555555', '#c6c6c6', '#5ea836', '#0f1f06', ['#7cd24a', '#d8a35a', '#5ec8e6'], 0, 3, 'pixel', 15, 'blocks'],
    ['comic', 'Comic', 'Comic', '#fffdf0', '#111111', '#ffffff', '#ffe14d', '#111111', '#fff3b0', '#4a4a4a', '#d11f1f', '#ffffff', ['#d11f1f', '#0077cc', '#c46a00'], 14, 3, '', 15, 'comic'],
    ['aurora', 'Aurora', 'Aurora', '#0a1020', '#e8f1ff', '#101a2e', '#070c18', '#22304d', '#162238', '#93a5c7', '#4ff0b0', '#04140d', ['#4ff0b0', '#b18cff', '#5cc8ff'], 10, 1, '', 14, 'aurora'],
    ['retro90', 'Retro 90s', 'Retro 90er', '#d4d0c8', '#000000', '#ffffff', '#c4c0b8', '#808080', '#c4c0b8', '#3a3a3a', '#000080', '#ffffff', ['#000080', '#800080', '#007070'], 0, 2, '', 14, 'memphis'],
    ['weltraum', 'Space', 'Weltraum', '#05060f', '#e9ecff', '#0c0f22', '#03040a', '#1f2547', '#121735', '#8e95c4', '#7c8cff', '#060818', ['#7c8cff', '#ff9d5c', '#5cf2c0'], 12, 1, '', 14, 'stars'],
    ['terminal', 'Terminal', 'Terminal', '#040904', '#3dff74', '#071207', '#020602', '#145a24', '#0b1f0f', '#22b14c', '#3dff74', '#021004', ['#3dff74', '#ffb000', '#4dd8ff'], 0, 1, 'term', 19, ''],
    ['nord', 'Nord', 'Nord', '#2e3440', '#eceff4', '#353c4a', '#292e39', '#434c5e', '#3b4252', '#a7b1c2', '#88c0d0', '#1d2129', ['#88c0d0', '#d08770', '#b48ead'], 8, 1, '', 14, ''],
    ['solarized', 'Solarized', 'Solarized', '#fdf6e3', '#4a5b62', '#fdf6e3', '#eee8d5', '#ddd6c1', '#eee8d5', '#839496', '#268bd2', '#fdf6e3', ['#268bd2', '#cb4b16', '#6c71c4'], 8, 1, '', 14, ''],
    ['papier', 'Paper', 'Papier', '#f5edda', '#3b2f26', '#faf4e6', '#ebdfc3', '#d9c8a3', '#ebdfc3', '#7d6a52', '#8b5a2b', '#fbf6ea', ['#8b5a2b', '#2f6f73', '#7a3e6a'], 6, 1, '', 14, ''],
    ['kontrast', 'High contrast', 'Hoher Kontrast', '#000000', '#ffffff', '#000000', '#000000', '#ffffff', '#262626', '#ffffff', '#ffd400', '#000000', ['#ffd400', '#5cd6ff', '#ff8cf2'], 4, 2, '', 15, ''],
    ['ozean', 'Ocean', 'Ozean', '#0b1e2d', '#e6f4f8', '#10283a', '#081824', '#1d4256', '#143247', '#8fb3c4', '#2ec4d6', '#04161d', ['#2ec4d6', '#ff9f68', '#a28bff'], 10, 1, '', 14, 'waves'],
    ['wald', 'Forest', 'Wald', '#18221a', '#eef2e6', '#1f2b21', '#131b15', '#33452f', '#26352a', '#a7b89b', '#8bc34a', '#0f1a08', ['#8bc34a', '#e7a35a', '#7fb8d8'], 8, 1, '', 14, 'leaves'],
    ['nachtschatten', 'Nightshade', 'Nachtschatten', '#1e1f29', '#f8f8f2', '#262836', '#191a22', '#3a3c4e', '#2e3040', '#a4a8c4', '#bd93f9', '#1a1029', ['#bd93f9', '#ff79c6', '#8be9fd'], 8, 1, '', 14, ''],
    ['kaffee', 'Coffee', 'Kaffee', '#f6efe6', '#3a2a20', '#fbf7f1', '#ecdfcf', '#dccab5', '#ecdfcf', '#7a6250', '#8a5a3b', '#fffaf4', ['#8a5a3b', '#2f6f6a', '#a14a6b'], 12, 1, '', 14, 'steam'],
    ['hell', 'Light', 'Hell', '#ffffff', '#17181c', '#ffffff', '#f7f7f8', '#e4e4e7', '#f1f1f3', '#6b6b74', '#0f7fb8', '#ffffff', ['#0f7fb8', '#c2570c', '#7c3aed'], 8, 1, '', 14, ''],
    ['dunkel', 'Dark', 'Dunkel', '#0b0b0d', '#ededef', '#141417', '#111113', '#27272b', '#1c1c20', '#9a9aa3', '#38b2ec', '#06121a', ['#38b2ec', '#f59e57', '#a78bfa'], 8, 1, '', 14, ''],
  ];
  const FONTS = { pixel: '"Pixelify Sans", var(--mono)', term: '"VT323", var(--mono)' };
  const themes = RAW.map(([id, name, native, bg, fg, sf, sb, bd, mu, mf, pr, pf, sp, rad, bw, font, fs, kind]) => (
    { id, name, native, bg, fg, sf, sb, bd, mu, mf, pr, pf, sp, rad, bw, font, fs, kind }
  ));
  const byId = Object.fromEntries(themes.map((t) => [t.id, t]));
  const SPIN = ['mostalgia-nacht', 'sakura', 'synthwave', 'blockwelt', 'comic', 'terminal', 'aurora'];

  // the pixel and terminal fonts are only needed here, so they load with the scene
  const fonts = document.createElement('link');
  fonts.rel = 'stylesheet';
  fonts.href = 'https://fonts.googleapis.com/css2?family=Pixelify+Sans:wght@500&family=VT323&display=swap';
  document.head.append(fonts);

  const win = $('#t9Win'), band = $('#t9Band'), wipe = $('#t9Wipe'), cassette = $('.t9-cassette', scene);
  const nameEl = $('#t9Name'), subEl = $('#t9Sub'), countEl = $('#t9Count'), themeVal = $('#t9Theme');
  const swatchBox = $('#t9Swatches'), tryBtn = $('#t9Try'), replay = $('#t9Replay');
  const size = $('#t9Size'), sizeOut = $('#t9SizeOut'), accent = $('#t9Accent'), hex = $('#t9Hex'), path = $('#t9Path');
  const langBtns = $$('.t9-seg button', win), labels = $$('[data-de]', win);
  const cursor = $('#t9Cursor');
  const rows = { size: $('.t9-r-size', win), lang: $('.t9-r-lang', win), own: $('.t9-r-own', win) };
  labels.forEach((l) => { l.dataset.en = l.textContent; });

  // ───────── colour helpers ─────────
  const rgb = (h) => [1, 3, 5].map((i) => parseInt(h.slice(i, i + 2), 16));
  const lum = (h) => {
    const [r, g, b] = rgb(h).map((v) => { v /= 255; return v <= 0.03928 ? v / 12.92 : ((v + 0.055) / 1.055) ** 2.4; });
    return 0.2126 * r + 0.7152 * g + 0.0722 * b;
  };
  const hsl2hex = (h, s, l) => {
    s /= 100; l /= 100;
    const k = (n) => (n + h / 30) % 12, a = s * Math.min(l, 1 - l);
    const f = (n) => Math.round(255 * (l - a * Math.max(-1, Math.min(k(n) - 3, 9 - k(n), 1))));
    return `#${[f(0), f(8), f(4)].map((v) => v.toString(16).padStart(2, '0')).join('')}`;
  };

  // ───────── little scenes (same ideas as the app's, drawn in CSS) ─────────
  let seed = 1;
  const rnd = () => { seed = (seed * 16807) % 2147483647; return (seed - 1) / 2147483646; };
  const SCENES = {
    steam: () => '<span class="t9-cup"><i></i><i></i><i></i></span>',
    petals: () => leaves(false),
    leaves: () => leaves(true),
    grid: () => '<span class="t9-sun"></span><span class="t9-floor"></span>',
    blocks: () => [0, 1, 2, 3, 4].map((i) => `<i class="t9-block" style="--i:${i}"></i>`).join(''),
    comic: () => '<span class="t9-bub"><i></i><i></i><i></i></span><i class="t9-bub-t"></i><i class="t9-bub-t sm"></i>',
    stars: () => Array.from({ length: 26 }, () => `<i class="t9-star" style="left:${(rnd() * 100).toFixed(1)}%;top:${(rnd() * 90).toFixed(1)}%;--s:${(1.5 + rnd() * 2.5).toFixed(1)}px;--d:${(0.8 + rnd() * 1.8).toFixed(2)}s;--dl:-${(rnd() * 2).toFixed(2)}s"></i>`).join('') + '<span class="t9-planet"></span>',
    aurora: () => [0, 1, 2].map((i) => `<i class="t9-aur" style="--i:${i};--c:var(--w-s${i})"></i>`).join(''),
    waves: () => [0, 1, 2, 3].map((i) => `<i class="t9-wave" style="--i:${i};left:${-12 + (i % 2) * 22}%;background:color-mix(in srgb, var(--w-pr) ${12 + i * 7}%, transparent)"></i>`).join(''),
    memphis: () => Array.from({ length: 9 }, (_, i) => `<i class="t9-memph ${['round', '', 'ring'][i % 3]}" style="left:${(4 + i * 10.5 + rnd() * 4).toFixed(1)}%;top:${(8 + rnd() * 52).toFixed(1)}%;--s:${Math.round(10 + rnd() * 16)}px;--c:var(--w-s${i % 3});--dl:-${(rnd() * 2.6).toFixed(2)}s"></i>`).join(''),
    term: () => '<span class="t9-prompt">&gt; </span>',
  };
  const leaves = (forest) => Array.from({ length: 11 }, (_, i) => `<i class="t9-leaf" style="left:${(rnd() * 96).toFixed(1)}%;--s:${Math.round(7 + rnd() * 6)}px;--d:${(3.2 + rnd() * 2.4).toFixed(2)}s;--dl:-${(rnd() * 5).toFixed(2)}s${forest ? `;--lc:color-mix(in srgb, var(--w-s${i % 2}) 55%, transparent)` : ''}"></i>`).join('');

  // ───────── state ─────────
  let current = byId.mostalgia, lang = 'en', mine = null, pageTheme = false;

  const paint = (t) => {
    const v = {
      bg: t.bg, fg: t.fg, sf: t.sf, sb: t.sb, bd: t.bd, mu: t.mu, mf: t.mf, pr: t.pr, pf: t.pf,
      s0: t.sp[0], s1: t.sp[1], s2: t.sp[2], rad: `${t.rad}px`, bw: `${t.bw}px`,
      font: FONTS[t.font] || 'var(--sans)', fs: `${(t.fs / 14 * 13).toFixed(2)}px`,
    };
    for (const [k, val] of Object.entries(v)) win.style.setProperty(`--w-${k}`, val);
    const kind = t.kind || (t.font === 'term' ? 'term' : '');
    if (band.dataset.scene !== kind) {
      seed = 7;
      band.dataset.scene = kind;
      band.innerHTML = SCENES[kind] ? SCENES[kind]() : '';
    }
    accent.value = t.pr.toLowerCase();
    hex.textContent = t.pr.toUpperCase();
    if (pageTheme) toPage(t.pr);
  };

  const label = (t) => {
    const i = themes.indexOf(t);
    nameEl.textContent = t.name;
    subEl.classList.remove('t9-nudge');
    subEl.textContent = t === mine ? `mine.json · based on ${mine.base}` : `"${t.id}" · ${t.kind || 'no'} scene`;
    countEl.textContent = t === mine ? 'NEW' : `${String(i + 1).padStart(2, '0')} / 20`;
    cassette.style.setProperty('--tc', t.pr);
    themeVal.textContent = lang === 'de' ? t.native : t.name;
    $$('.t9-sw', swatchBox).forEach((b) => b.setAttribute('aria-pressed', String(b.dataset.t === t.id)));
  };

  // the cassette's small print doubles as the "your turn" nudge
  const hint = (text) => { subEl.textContent = text; subEl.classList.add('t9-nudge'); };

  const restart = (el, c) => { el.classList.remove(c); void el.offsetWidth; el.classList.add(c); };

  let swapTimer = 0;
  const pick = (t, instant) => {
    current = t;
    label(t);
    clearTimeout(swapTimer);
    if (instant || reduce) { paint(t); return; }
    [nameEl, subEl].forEach((el) => restart(el, 'swap'));
    restart(cassette, 'spin');
    for (const k of ['bg', 'fg', 'pr']) wipe.style.setProperty(`--n-${k}`, t[k]);
    restart(wipe, 'go');
    swapTimer = setTimeout(() => paint(t), 300);
  };
  wipe.addEventListener('animationend', () => wipe.classList.remove('go'));
  // drop one-shot classes once played, so a layout change (e.g. the sidebar reappearing) can't replay them
  scene.addEventListener('animationend', (e) => e.target.classList.remove('flip', 'tap', 'swap'));

  const setSize = (v) => {
    win.style.setProperty('--t9-scale', v / 100);
    size.value = v;
    sizeOut.textContent = `${Math.round(v / 10) * 10} %`;
  };

  const setLang = (l) => {
    lang = l;
    win.lang = l;
    labels.forEach((el) => {
      el.textContent = el.dataset[l];
      if (!reduce) restart(el, 'flip');
    });
    langBtns.forEach((b) => b.setAttribute('aria-pressed', String(b.dataset.lang === l)));
    themeVal.textContent = l === 'de' ? current.native : current.name;
  };

  // "Make your own": the current theme with a new accent, saved as mine.json
  const makeMine = (pr) => {
    if (!mine || current !== mine) {
      mine = { ...current, id: 'mine', name: 'My theme', native: 'Mein Design', base: current.name };
      if (!$('.t9-sw[data-t=mine]', swatchBox)) swatchBox.append(swatch(mine, true));
      current = mine;
    }
    mine.pr = pr;
    mine.sp = [pr, mine.sp[1], mine.sp[2]];
    mine.pf = lum(pr) > 0.4 ? '#111111' : '#ffffff';
    clearTimeout(swapTimer);
    paint(mine);
    label(mine);
    $('.t9-sw[data-t=mine]', swatchBox).style.cssText = `--b:${mine.bg};--a:${pr};--c:${mine.sp[1]}`;
  };
  const saved = () => {
    path.textContent = '~/.config/mostalgia/themes/mine.json';
    if (!reduce) restart(path, 'saved'); else path.classList.add('saved');
  };

  // "Try it on this page": the site's own accents follow the window (kept readable)
  const root = document.documentElement.style;
  const PAGE_VARS = ['--amber', '--amber-l', '--terra', '--ember'];
  const toPage = (pr) => {
    const l = lum(pr);
    const amber = l < 0.3 ? `color-mix(in oklab, ${pr} 55%, #fff)` : pr;
    const terra = l > 0.2 ? `color-mix(in oklab, ${pr} 65%, #2B1730)` : pr;
    root.setProperty('--amber', amber);
    root.setProperty('--amber-l', `color-mix(in oklab, ${amber} 60%, #fff)`);
    root.setProperty('--terra', terra);
    root.setProperty('--ember', `color-mix(in oklab, ${terra} 80%, #2B1730)`);
  };

  // ───────── swatches ─────────
  const swatch = (t, isNew) => {
    const b = document.createElement('button');
    b.type = 'button';
    b.className = `t9-sw${isNew ? ' mine' : ''}`;
    b.dataset.t = t.id;
    b.title = t.name;
    b.setAttribute('aria-label', t.name);
    b.setAttribute('aria-pressed', String(t === current));
    b.style.cssText = `--b:${t.bg};--a:${t.pr};--c:${t.sp[1]}`;
    return b;
  };
  themes.forEach((t) => swatchBox.append(swatch(t)));

  // ───────── the visitor's controls ─────────
  [size, accent, ...langBtns].forEach((el) => { el.disabled = false; });
  tryBtn.hidden = false;
  let demo = false; // true while the autoplay is driving
  const takeOver = () => {
    if (!demo) return;
    stopDemo();
    if (!reduce) replay.hidden = false;
  };
  swatchBox.addEventListener('click', (e) => {
    const b = e.target.closest('.t9-sw');
    if (!b) return;
    takeOver();
    if (b.dataset.t === current.id) return;
    pick(b.dataset.t === 'mine' ? mine : byId[b.dataset.t]);
  });
  size.addEventListener('input', () => { takeOver(); setSize(+size.value); });
  langBtns.forEach((b) => b.addEventListener('click', () => { takeOver(); setLang(b.dataset.lang); }));
  accent.addEventListener('input', () => { takeOver(); makeMine(accent.value); });
  accent.addEventListener('change', () => saved());
  tryBtn.addEventListener('click', () => {
    pageTheme = !pageTheme;
    tryBtn.setAttribute('aria-pressed', String(pageTheme));
    if (pageTheme) toPage(current.pr);
    else PAGE_VARS.forEach((v) => root.removeProperty(v));
  });
  // the app's own shortcuts work while focus is inside the window
  win.addEventListener('keydown', (e) => {
    if (!e.ctrlKey || !['=', '+', '-', '0'].includes(e.key)) return;
    e.preventDefault();
    takeOver();
    const v = e.key === '0' ? 100 : Math.max(90, Math.min(150, +size.value + (e.key === '-' ? -10 : 10)));
    setSize(v);
  });

  if (reduce) { hint('▾ pick a theme below'); return; }

  // ───────── autoplay, on a clock that only runs while the scene is on screen ─────────
  const rel = (el, fx = 0.5, fy = 0.5) => {
    const a = el.getBoundingClientRect(), s = scene.getBoundingClientRect();
    return [a.left - s.left + a.width * fx, a.top - s.top + a.height * fy];
  };
  const point = ([x, y], slow) => {
    cursor.classList.toggle('drag', !!slow);
    cursor.style.left = `${x - 4}px`;
    cursor.style.top = `${y - 3}px`;
    cursor.classList.add('on');
  };
  const thumb = (v) => {
    const a = size.getBoundingClientRect(), s = scene.getBoundingClientRect();
    return [a.left - s.left + 8 + (a.width - 16) * (v - 90) / 60, a.top - s.top + a.height / 2];
  };
  const click = () => restart(cursor, 'click');
  const hot = (k) => Object.entries(rows).forEach(([n, r]) => r.classList.toggle('hot', n === k));

  let events = [], tweens = [], next = 0, t = 0, last = 0, raf = 0, visible = false, started = false;
  const stopDemo = () => {
    demo = false;
    next = events.length;
    tweens = [];
    cursor.classList.remove('on');
    hot(null);
  };

  const reset = () => {
    clearTimeout(swapTimer);
    $('.t9-sw[data-t=mine]', swatchBox)?.remove();
    mine = null;
    path.textContent = '~/.config/mostalgia/themes/';
    path.classList.remove('saved');
    pick(byId.mostalgia, true);
    setLang('en');
    setSize(100);
    replay.hidden = true;
    t = 0; next = 0; tweens = []; demo = true;

    const ev = [];
    const at = (time, fn) => ev.push({ time, fn });
    const tween = (t0, t1, fn) => at(t0, () => tweens.push({ t0, t1, fn }));
    at(0, () => point(rel(swatchBox, 0.5, 1.15)));
    SPIN.forEach((id, i) => {
      const go = 0.9 + i * 1.5;
      const sw = () => $(`.t9-sw[data-t="${id}"]`, swatchBox);
      at(go - 0.6, () => point(rel(sw())));
      at(go, () => { click(); restart(sw(), 'tap'); pick(byId[id]); });
    });
    // text size: drag the slider up to 140 % and settle on 110 %
    at(10.6, () => { hot('size'); point(thumb(100)); });
    at(11.2, click);
    at(11.3, () => point(thumb(140), true));
    tween(11.3, 12.3, (p) => setSize(100 + 40 * p));
    at(12.7, () => point(thumb(110), true));
    tween(12.7, 13.4, (p) => setSize(140 - 30 * p));
    // language: German, labels flip
    at(13.8, () => { hot('lang'); point(rel(langBtns[1])); });
    at(14.4, () => { click(); setLang('de'); });
    // make your own: nudge the accent round the colour wheel into mine.json
    at(15.3, () => { hot('own'); point(rel(accent)); });
    at(15.9, () => { click(); makeMine(current.pr); });
    tween(16.0, 17.6, (p) => makeMine(hsl2hex(157 + 173 * p, 85, 62)));
    at(17.9, saved);
    at(18.7, () => { hot(null); point(rel(langBtns[0])); });
    at(19.3, () => { click(); setLang('en'); });
    at(20.0, () => { cursor.classList.remove('on'); hint('▾ your turn: pick one'); });
    at(20.4, () => { demo = false; replay.hidden = false; });
    events = ev.sort((x, y) => x.time - y.time);
  };

  const frame = (now) => {
    raf = 0;
    t += Math.min(0.1, (now - last) / 1000);
    last = now;
    while (next < events.length && events[next].time <= t) events[next++].fn();
    tweens = tweens.filter((w) => {
      const p = Math.min(1, (t - w.t0) / (w.t1 - w.t0));
      w.fn(p);
      return p < 1;
    });
    if (visible && (next < events.length || tweens.length)) raf = requestAnimationFrame(frame);
  };
  const play = () => {
    scene.classList.remove('t9-paused');
    if (!raf && (next < events.length || tweens.length)) { last = performance.now(); raf = requestAnimationFrame(frame); }
  };
  const pause = () => { cancelAnimationFrame(raf); raf = 0; scene.classList.add('t9-paused'); };

  reset();
  // start once ~40% of the scene (or of the screen, on tall phones) is in view; pause whenever it leaves
  new IntersectionObserver(([e]) => {
    const vh = e.rootBounds ? e.rootBounds.height : innerHeight;
    const seen = e.intersectionRect.height / Math.min(e.boundingClientRect.height, vh);
    visible = started ? e.isIntersecting : seen >= 0.4;
    if (visible) { started = true; play(); } else pause();
  }, { threshold: [0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1] }).observe(scene);

  replay.addEventListener('click', () => { pause(); reset(); started = visible = true; play(); });
})();
