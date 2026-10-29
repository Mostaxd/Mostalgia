// Episode 2 · live transcription. The HTML holds the finished scene (what you
// see without JS or with reduced motion); here it is rewound and replayed on a
// clock that only runs while the scene is on screen.
(() => {
  const site = window.MostalgiaSite;
  const scene = document.getElementById('e2Scene');
  if (!site || !scene) return;
  const { reduce, $, $$, sprite, type, preload } = site;

  const hoshi = $('#e2Hoshi'), actor = hoshi.closest('.actor'), say = $('#e2Say');
  const finalSay = say.textContent;
  // main.js puts the companion in the pose of the episode's first step; the finished scene ends idle
  if (reduce) { hoshi.src = sprite('hoshi', 'idle'); return; }

  const box = $('#e2Lines'), lines = $$('.e2-line', box);
  const lang = $('#e2Lang'), ontop = $('.e2-ontop', scene);
  const opts = Object.fromEntries($$('.e2-opt', scene).map((o) => [o.dataset.mode, o]));
  const engine = $('#e2Engine'), eng = $('#e2Eng'), where = $('#e2Where'), lat = $('#e2Lat');
  const segs = $$('.e2-seg', scene), slot = $('#e2Slot'), err = $('#e2Err'), stamp = $('.e2-stamp', err);
  const tiles = Object.fromEntries($$('.e2-tile', scene).map((t) => [t.dataset.who, t]));
  const replay = $('#e2Replay');
  const texts = lines.map((l) => { const t = $('.e2-t', l); return { t, text: t.textContent }; });
  const ENGINES = { local: ['Whisper turbo', 'your GPU · free'], cloud: ['Cloud Whisper', 'hosted · ≈ $0.36 / h'] };

  const restart = (el, c) => { el.classList.remove(c); void el.offsetWidth; el.classList.add(c); };
  const pose = (p, line) => {
    const src = sprite('hoshi', p);
    if (hoshi.getAttribute('src') !== src) { hoshi.src = src; restart(actor, 'hop'); }
    if (line) type(say, line);
  };
  const setEngine = (mode) => {
    Object.entries(opts).forEach(([m, o]) => o.classList.toggle('on', m === mode));
    [eng.textContent, where.textContent] = ENGINES[mode];
    restart(engine, 'swap');
  };
  const setLat = (v, cls) => { lat.textContent = v; lat.className = ''; if (cls) restart(lat, cls); };
  const press = (o, t, fn) => [[t - 0.25, () => o.classList.add('press')], [t, () => { o.classList.remove('press'); fn(); }]];

  // the caption box keeps its finished height, so the window never resizes while lines arrive
  const lock = () => {
    box.style.height = '';
    scene.classList.remove('e2-anim');
    box.style.height = `${box.offsetHeight + 4}px`;
    scene.classList.add('e2-anim');
  };

  // a new caption pushes the older ones up (FLIP), and its words resolve out of a blur
  const showLine = (li, i) => {
    const shown = lines.filter((l) => l.classList.contains('on'));
    const before = shown.map((l) => l.offsetTop);
    const { t, text } = texts[i];
    t.textContent = '';
    text.split(' ').forEach((w, k) => {
      const s = document.createElement('span');
      s.className = 'e2-w';
      s.textContent = w;
      s.style.animationDelay = `${k * 0.1}s`;
      t.append(s, k < text.split(' ').length - 1 ? ' ' : '');
    });
    li.classList.add('on');
    shown.forEach((l, k) => {
      if (!l.classList.contains('ghost')) l.classList.add('old');
      const d = before[k] - l.offsetTop;
      if (d) l.animate([{ transform: `translateY(${d}px)` }, { transform: 'none' }], { duration: 420, easing: 'cubic-bezier(.2,.9,.3,1)' });
    });
    li.animate([{ opacity: 0, transform: 'translateY(14px)' }, { opacity: 1, transform: 'none' }], { duration: 380, easing: 'ease-out' });
  };

  let events = [], next = 0, t = 0, last = 0, raf = 0;
  let started = false, visible = false;

  const reset = () => {
    lock();
    lines.forEach((l, i) => { l.classList.remove('on', 'old', 'struck', 'skipped'); texts[i].t.textContent = texts[i].text; });
    segs.forEach((s) => { s.classList.remove('done'); $$('i', s).forEach((b) => b.classList.remove('on')); });
    Object.values(tiles).forEach((tl) => tl.classList.remove('talk'));
    Object.values(opts).forEach((o) => o.classList.remove('press', 'fail'));
    Object.entries(opts).forEach(([m, o]) => o.classList.toggle('on', m === 'local'));
    [eng.textContent, where.textContent] = ENGINES.local;
    setLat('—');
    lang.textContent = '··';
    err.classList.remove('in');
    slot.classList.remove('err-on');
    stamp.classList.remove('slam');
    replay.hidden = true;
    hoshi.src = sprite('hoshi', 'listening');
    say.textContent = 'Captions on. They float above the call.';
    t = 0; next = 0;

    const ev = [];
    const at = (time, fn) => ev.push({ time, fn });
    at(0.3, () => restart(ontop, 'wiggle'));

    // who is talking: their tile lights up and the voice-activity lane fills in
    segs.forEach((s) => {
      const from = +s.dataset.from, to = +s.dataset.to, bars = $$('i', s), tile = tiles[[...s.classList].find((c) => tiles[c])];
      bars.forEach((b, k) => at(from + ((to - from) * k) / bars.length, () => b.classList.add('on')));
      at(to, () => s.classList.add('done'));
      if (tile) { at(from, () => tile.classList.add('talk')); at(to, () => tile.classList.remove('talk')); }
    });

    // captions arrive about a second after the words; the language chip flips by itself
    const LAT = ['0.9 s', '1.1 s', '0.8 s'];
    lines.forEach((l, i) => {
      const a = +l.dataset.at;
      at(a, () => {
        showLine(l, i);
        if (l.dataset.lang && lang.textContent !== l.dataset.lang) { lang.textContent = l.dataset.lang; restart(lang, 'flip'); }
      });
      if (LAT[i]) at(a + 0.5, () => setLat(LAT[i], 'tick'));
    });
    const ghost = lines.find((l) => l.classList.contains('ghost'));
    at(+ghost.dataset.at + 0.8, () => ghost.classList.add('struck'));
    at(+ghost.dataset.at + 1.1, () => ghost.classList.add('skipped'));

    at(2.2, () => pose('writing', 'German. Detected, not picked.'));
    at(5.2, () => pose('listening', 'English now, about a second behind.'));
    at(8.0, () => pose('talking', 'Arabic, right to left. Nobody had to choose.'));
    at(9.8, () => pose('thinking', 'Shh… nobody said that.'));
    at(11.1, () => pose('writing', 'Skipped. Silence stays silence.'));

    // the engine switch: Cloud fails out loud, and only you switch back
    press(opts.cloud, 12.2, () => { setEngine('cloud'); setLat('…', 'wait'); pose('thinking', 'Trying the cloud engine…'); }).forEach(([x, f]) => at(x, f));
    at(13.3, () => { err.classList.add('in'); slot.classList.add('err-on'); opts.cloud.classList.add('fail'); setLat('—'); pose('error', 'Unreachable. I tell you; I don\'t switch behind your back.'); });
    at(13.7, () => restart(stamp, 'slam'));
    press(opts.local, 15.4, () => { opts.cloud.classList.remove('fail'); setEngine('local'); setLat('0.9 s', 'tick'); }).forEach(([x, f]) => at(x, f));
    at(15.8, () => pose('idle', finalSay));
    at(16.8, () => { replay.hidden = false; });
    events = ev.sort((x, y) => x.time - y.time);
  };

  const frame = (now) => {
    raf = 0;
    t += Math.min(0.5, (now - last) / 1000);
    last = now;
    while (next < events.length && events[next].time <= t) events[next++].fn();
    if (visible && next < events.length) raf = requestAnimationFrame(frame);
  };
  const play = () => {
    scene.classList.remove('e2-paused');
    if (!raf && next < events.length) { last = performance.now(); raf = requestAnimationFrame(frame); }
  };
  const pause = () => { cancelAnimationFrame(raf); raf = 0; scene.classList.add('e2-paused'); };

  reset();
  ['listening', 'writing', 'talking', 'thinking', 'error', 'idle'].forEach((p) => preload(sprite('hoshi', p)));

  // start once ~40% of the scene (or of the screen, on tall phones) is in view; pause whenever it leaves
  new IntersectionObserver(([e]) => {
    const vh = e.rootBounds ? e.rootBounds.height : innerHeight;
    const seen = e.intersectionRect.height / Math.min(e.boundingClientRect.height, vh);
    visible = started ? e.isIntersecting : seen >= 0.4;
    if (visible) { started = true; play(); } else pause();
  }, { threshold: [0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1] }).observe(scene);

  // the finished height depends on the width, so measure again after a resize
  let rt = 0;
  addEventListener('resize', () => {
    clearTimeout(rt);
    rt = setTimeout(lock, 200);
  });

  replay.addEventListener('click', () => { pause(); reset(); started = visible = true; play(); });
})();
