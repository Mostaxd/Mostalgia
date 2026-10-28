// Mostalgia site: petals, hero parallax, scroll-driven companions, cast select.
(() => {
  const reduce = matchMedia('(prefers-reduced-motion: reduce)').matches;
  const $ = (s, el = document) => el.querySelector(s);
  const $$ = (s, el = document) => [...el.querySelectorAll(s)];
  const sprite = (c, p) => `assets/cast/${c}-${p}.webp`;

  // ───────── title letters pop in ─────────
  const word = $('.title .word');
  if (word && !reduce) {
    const text = word.textContent;
    word.textContent = '';
    word.setAttribute('aria-label', text);
    [...text].forEach((ch, i) => {
      const s = document.createElement('span');
      s.className = 'ch';
      s.textContent = ch;
      s.style.animationDelay = `${0.15 + i * 0.06}s`;
      s.setAttribute('aria-hidden', 'true');
      word.appendChild(s);
    });
  }

  // ───────── sakura petals ─────────
  const cv = $('#petals');
  if (cv && !reduce) {
    const ctx = cv.getContext('2d');
    let W, H, dpr;
    const petals = [];
    const resize = () => {
      dpr = Math.min(devicePixelRatio || 1, 2);
      W = cv.width = innerWidth * dpr;
      H = cv.height = innerHeight * dpr;
    };
    resize();
    addEventListener('resize', resize);
    const count = innerWidth < 700 ? 14 : 26;
    const make = (initial) => ({
      x: Math.random() * W,
      y: initial ? Math.random() * H : -20 * dpr,
      s: (6 + Math.random() * 7) * dpr,
      vy: (0.35 + Math.random() * 0.6) * dpr,
      vx: (0.2 + Math.random() * 0.5) * dpr,
      a: Math.random() * Math.PI * 2,
      va: 0.01 + Math.random() * 0.03,
      flip: Math.random() * Math.PI * 2,
      hue: Math.random() < 0.7 ? '#F8C3CF' : '#F7C26B',
    });
    for (let i = 0; i < count; i++) petals.push(make(true));
    let scrollBoost = 0, lastY = scrollY;
    addEventListener('scroll', () => { scrollBoost = Math.min(6, scrollBoost + Math.abs(scrollY - lastY) * 0.02); lastY = scrollY; }, { passive: true });
    const draw = () => {
      ctx.clearRect(0, 0, W, H);
      scrollBoost *= 0.94;
      for (const p of petals) {
        p.y += p.vy * (1 + scrollBoost);
        p.x += p.vx + Math.sin(p.a) * 0.6 * dpr;
        p.a += p.va;
        p.flip += 0.04;
        if (p.y > H + 20 * dpr || p.x > W + 20 * dpr) Object.assign(p, make(false), { x: Math.random() * W * 0.9 - W * 0.2 });
        ctx.save();
        ctx.translate(p.x, p.y);
        ctx.rotate(p.a);
        ctx.scale(1, Math.abs(Math.cos(p.flip)) * 0.7 + 0.3);
        ctx.globalAlpha = 0.85;
        ctx.fillStyle = p.hue;
        ctx.beginPath();
        ctx.moveTo(0, -p.s);
        ctx.bezierCurveTo(p.s * 0.9, -p.s * 0.6, p.s * 0.7, p.s * 0.6, 0, p.s);
        ctx.bezierCurveTo(-p.s * 0.7, p.s * 0.6, -p.s * 0.9, -p.s * 0.6, -p.s * 0.15, -p.s * 0.85);
        ctx.lineTo(0, -p.s * 0.6);
        ctx.closePath();
        ctx.fill();
        ctx.restore();
      }
      requestAnimationFrame(draw);
    };
    requestAnimationFrame(draw);
  }

  // ───────── top bar + hero parallax + rail ─────────
  const topbar = $('#topbar');
  const hero = $('#hero');
  const sun = $('.sun');
  const lineup = $$('#lineup figure');
  const rail = $('#rail');
  let ticking = false;
  const onScroll = () => {
    ticking = false;
    const y = scrollY;
    topbar.classList.toggle('solid', y > 40);
    const h = hero.offsetHeight;
    const end = document.getElementById('numbers');
    rail.classList.toggle('show', y > h * 0.8 && y < end.offsetTop - innerHeight * 0.5);
    if (reduce || y > h * 1.2) return;
    const p = Math.min(1, y / h);
    if (sun) sun.style.transform = `translate(-50%, ${p * 45}%)`;
    // the cast leaps apart as you scroll down, as if heading off to their episodes
    lineup.forEach((f, i) => {
      const d = +f.dataset.depth;
      const dir = i - 2;
      f.style.transform = `translate(${dir * p * 90 * d}px, ${-p * 160 * d}px) rotate(${dir * p * 8}deg)`;
    });
  };
  addEventListener('scroll', () => { if (!ticking) { ticking = true; requestAnimationFrame(onScroll); } }, { passive: true });
  onScroll();

  // ───────── reveal on scroll ─────────
  const revealer = new IntersectionObserver((entries) => {
    for (const e of entries) if (e.isIntersecting) { e.target.classList.add('in'); revealer.unobserve(e.target); }
  }, { threshold: 0.2 });
  $$('.reveal, .ep-card').forEach((el) => revealer.observe(el));

  // ───────── typewriter ─────────
  const typers = new WeakMap();
  const type = (el, text) => {
    clearInterval(typers.get(el));
    if (reduce) { el.textContent = text; return; }
    el.textContent = '';
    const caret = document.createElement('span');
    caret.className = 'caret';
    const node = document.createTextNode('');
    el.append(node, caret);
    let i = 0;
    const t = setInterval(() => {
      i += 1;
      node.data = text.slice(0, i);
      if (i >= text.length) { clearInterval(t); setTimeout(() => caret.remove(), 1200); }
    }, 22);
    typers.set(el, t);
  };

  // ───────── episodes: the companion changes pose step by step ─────────
  const mini = $('#miniGuide');
  const miniImg = $('img', mini);
  const miniSay = $('.say', mini);
  const preloaded = new Set();
  const preload = (src) => { if (!preloaded.has(src)) { preloaded.add(src); const i = new Image(); i.src = src; } };

  const episodes = $$('.episode[data-char]');
  episodes.forEach((ep) => {
    const char = ep.dataset.char;
    const stage = $('.stage', ep);
    const actor = $('.actor', stage);
    const img = $('img', actor);
    const say = $('.say', stage);
    const steps = $$('.step', ep);
    let current = null;

    const activate = (step) => {
      if (step === current) return;
      current = step;
      steps.forEach((s) => s.classList.toggle('on', s === step));
      const src = sprite(char, step.dataset.pose);
      const changed = img.getAttribute('src') !== src;
      img.src = src;
      if (changed && !reduce) { actor.classList.remove('hop'); void actor.offsetWidth; actor.classList.add('hop'); }
      type(say, step.dataset.say);
      if (mini.dataset.ep === ep.id) showMini(char, step);
      // warm up the next pose so the swap is instant
      const next = steps[steps.indexOf(step) + 1];
      if (next) preload(sprite(char, next.dataset.pose));
    };
    ep._activate = activate;
    ep._first = () => current || steps[0];
    img.src = sprite(char, steps[0].dataset.pose);

    const io = new IntersectionObserver((entries) => {
      for (const e of entries) if (e.isIntersecting) activate(e.target);
    }, { rootMargin: '-45% 0px -45% 0px' });
    steps.forEach((s) => io.observe(s));

    // a gentle sway while scrolling through the episode
    if (!reduce) {
      const sway = () => {
        const r = ep.getBoundingClientRect();
        if (r.bottom < 0 || r.top > innerHeight) return;
        const p = (innerHeight - r.top) / (r.height + innerHeight);
        actor.style.translate = `${Math.sin(p * Math.PI * 6) * 10}px ${Math.abs(Math.sin(p * Math.PI * 12)) * -6}px`;
      };
      addEventListener('scroll', sway, { passive: true });
    }
  });

  // mobile: one small companion in the corner follows whichever episode is on screen
  const showMini = (char, step) => {
    const src = sprite(char, step.dataset.pose);
    if (miniImg.getAttribute('src') !== src) miniImg.src = src;
    type(miniSay, step.dataset.say);
  };
  const epWatch = new IntersectionObserver((entries) => {
    for (const e of entries) {
      const ep = e.target.parentElement;
      if (e.isIntersecting) {
        mini.dataset.ep = ep.id;
        mini.classList.add('show');
        showMini(ep.dataset.char, ep._first());
      } else if (mini.dataset.ep === ep.id) {
        mini.classList.remove('show');
        mini.dataset.ep = '';
      }
    }
  }, { rootMargin: '-35% 0px -35% 0px' });
  episodes.forEach((ep) => epWatch.observe($('.ep-body', ep)));

  // ───────── episode rail ─────────
  const railLinks = $$('#rail a');
  const sections = railLinks.map((a) => document.getElementById(a.dataset.ep));
  const railIO = new IntersectionObserver((entries) => {
    for (const e of entries) if (e.isIntersecting) {
      railLinks.forEach((a) => a.classList.toggle('on', a.dataset.ep === e.target.id));
    }
  }, { rootMargin: '-50% 0px -50% 0px' });
  sections.forEach((s) => s && railIO.observe(s));

  // ───────── live transcript demo ─────────
  const ld = $('#ldType');
  if (ld) {
    const words = "Okay. Then I'd suggest we freeze marketing spend until the end of June.".split(' ');
    let running = false;
    const run = () => {
      if (running) return;
      running = true;
      ld.className = 'ld-text prov';
      ld.textContent = '';
      let i = 0;
      const t = setInterval(() => {
        i += 1;
        // provisional text arrives in bursts, slightly off, then the clean line replaces it
        ld.textContent = words.slice(0, i).join(' ').replace('freeze', i < 9 ? 'free' : 'freeze') + (i < words.length ? ' …' : '');
        if (i >= words.length) {
          clearInterval(t);
          setTimeout(() => { ld.className = 'ld-text final'; ld.textContent = words.join(' '); }, 700);
          setTimeout(() => { running = false; }, 4200);
        }
      }, reduce ? 0 : 260);
    };
    new IntersectionObserver((es) => es.forEach((e) => e.isIntersecting && run()), { threshold: 0.6 }).observe($('#liveDemo'));
    setInterval(() => { const r = ld.getBoundingClientRect(); if (r.top > 0 && r.bottom < innerHeight) run(); }, 6000);
  }

  // ───────── cast select ─────────
  const CAST = {
    mori: {
      kana: 'モリ', role: 'The note-taker', tag: 'Takes notes on her notepad',
      bio: "Warm, calm, a little witty and concise. Mori writes along whenever someone speaks, pins up what she noticed, and remembers what was said. Her name comes from memory.",
      notes: 'assets/shots/mori-points.webp',
    },
    tsumugi: {
      kana: 'ツムギ', role: 'The weaver', tag: 'Weaves the notes into her scarf',
      bio: "Patient, gentle and unhurried, even when the meeting isn't. Tsumugi spins what people say into thread and weaves each note into her long scarf, picking up threads and tying off loose ends.",
      notes: 'assets/cast/tsumugi-notes.webp',
    },
    hoshi: {
      kana: 'ホシ', role: 'The astronomer', tag: 'Charts the notes as constellations',
      bio: "Curious, calm and precise, glad to notice how things connect. Hoshi sits with her telescope and star chart and maps every note as a star, so you can see how the points line up.",
      notes: 'assets/cast/hoshi-notes.webp',
    },
    kaito: {
      kana: 'カイト', role: 'The tea host', tag: 'Serves each note like a cup of tea',
      bio: "Calm, polite and gently hospitable, attentive to what you need. Kaito arrives with a tray and teapot and serves each note like a freshly poured cup, lined up on the shelf.",
      notes: 'assets/cast/kaito-notes.webp',
    },
    jin: {
      kana: 'ジン', role: 'The wandering swordsman', tag: 'Cuts each note onto a paper strip',
      bio: "Stoic, terse and direct. Jin cuts straight to the point, never rude, and slices each note cleanly onto a paper strip.",
      notes: 'assets/cast/jin-notes.webp',
    },
  };
  const sel = $('#castSelect');
  if (sel) {
    const pImg = $('#pImg'), pName = $('#pName'), pKana = $('#pKana'), pRole = $('#pRole'), pTag = $('#pTag'), pBio = $('#pBio'), pNotes = $('#pNotes');
    const tabs = $$('.roster button', sel);
    const poseBtns = $$('.poses button', sel);
    let who = 'mori', pose = 'idle', touched = false;

    const show = () => {
      const src = sprite(who, pose);
      pImg.classList.add('swap');
      const img = new Image();
      img.onload = () => {
        pImg.src = src;
        pImg.alt = `${who[0].toUpperCase() + who.slice(1)}, ${pose}`;
        requestAnimationFrame(() => pImg.classList.remove('swap'));
      };
      img.src = src;
      poseBtns.forEach((b) => b.classList.toggle('on', b.dataset.p === pose));
    };
    const pick = (c) => {
      who = c;
      const d = CAST[c];
      tabs.forEach((t) => t.setAttribute('aria-selected', String(t.dataset.c === c)));
      pName.textContent = c[0].toUpperCase() + c.slice(1);
      pKana.textContent = d.kana;
      pRole.textContent = d.role;
      pTag.textContent = d.tag;
      pBio.textContent = d.bio;
      pNotes.src = d.notes;
      pNotes.alt = `How ${pName.textContent} shows notes`;
      show();
    };
    tabs.forEach((t) => t.addEventListener('click', () => { touched = true; pick(t.dataset.c); }));
    poseBtns.forEach((b) => b.addEventListener('click', () => { touched = true; pose = b.dataset.p; show(); }));
    pick('mori');

    // until someone clicks, the cast takes turns showing off their poses
    const order = ['idle', 'listening', 'writing', 'thinking', 'talking', 'sleeping', 'error'];
    const cast = Object.keys(CAST);
    let visible = false, tick = 0;
    new IntersectionObserver((es) => es.forEach((e) => { visible = e.isIntersecting; }), { threshold: 0.4 }).observe(sel);
    if (!reduce) setInterval(() => {
      if (touched || !visible) return;
      tick += 1;
      if (tick % 3 === 0) { pose = 'idle'; pick(cast[(cast.indexOf(who) + 1) % cast.length]); }
      else { pose = order[1 + Math.floor(Math.random() * (order.length - 1))]; show(); }
    }, 2600);
  }

  // ───────── theme marquee ─────────
  const THEMES = [
    ['mostalgia', 'Mostalgia'], ['mostalgia-nacht', 'Mostalgia Night'], ['sakura', 'Sakura'], ['synthwave', 'Synthwave'],
    ['blockwelt', 'Blockworld'], ['comic', 'Comic'], ['aurora', 'Aurora'], ['retro90', 'Retro 90s'], ['weltraum', 'Space'],
    ['terminal', 'Terminal'], ['nord', 'Nord'], ['solarized', 'Solarized'], ['papier', 'Paper'], ['kontrast', 'High contrast'],
    ['ozean', 'Ocean'], ['wald', 'Forest'], ['nachtschatten', 'Nightshade'], ['kaffee', 'Coffee'], ['hell', 'Light'], ['dunkel', 'Dark'],
  ];
  const fill = (el, list) => {
    if (!el) return;
    const html = list.map(([id, name]) => `<figure class="tcard"><img src="assets/themes/${id}.webp" alt="${name} theme" loading="lazy" width="640" height="423"><figcaption>${name}</figcaption></figure>`).join('');
    el.innerHTML = html + html.replace(/alt="[^"]*"/g, 'alt=""');
  };
  fill($('#trackA'), THEMES.slice(0, 10));
  fill($('#trackB'), THEMES.slice(10));

  // ───────── counters ─────────
  const counter = new IntersectionObserver((entries) => {
    for (const e of entries) {
      if (!e.isIntersecting) continue;
      counter.unobserve(e.target);
      const b = e.target;
      const to = +b.dataset.count, pre = b.dataset.prefix || '', suf = b.dataset.suffix || '';
      if (reduce) continue;
      const t0 = performance.now();
      const step = (t) => {
        const k = Math.min(1, (t - t0) / 1200);
        b.textContent = pre + Math.round(to * (1 - Math.pow(1 - k, 3))) + suf;
        if (k < 1) requestAnimationFrame(step);
      };
      requestAnimationFrame(step);
    }
  }, { threshold: 0.6 });
  $$('[data-count]').forEach((b) => counter.observe(b));

  // ───────── copy install ─────────
  const copyBtn = $('#copyBtn');
  copyBtn?.addEventListener('click', async () => {
    const text = $('#installCmd').innerText.split('\n').filter((l) => l.trim() && !l.trim().startsWith('#')).join('\n');
    try { await navigator.clipboard.writeText(text); copyBtn.textContent = 'Copied!'; }
    catch { copyBtn.textContent = 'Select & copy'; }
    setTimeout(() => { copyBtn.textContent = 'Copy'; }, 1800);
  });
})();
