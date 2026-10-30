// Episode 5 · Ask: pick a question, watch it searched across every meeting, answered
// with sources, and opened at the exact line. The HTML holds one finished answer
// (what you see without JS or with reduced motion); here it is rewound and replayed
// on a clock that only runs while the scene is on screen.
(() => {
  const site = window.MostalgiaSite;
  const scene = document.getElementById('e5Scene');
  if (!site || !scene) return;
  const { reduce, $, $$, sprite, type, preload } = site;

  // ───── canned data (fictional) ─────
  const MEETINGS = {
    spring: {
      title: 'Spring launch sync', when: 'Today, 10:00', lines: [
        ['02:01', 'sam', 'Sam', 'Quick one today, I have a hard stop at half past.'],
        ['02:04', 'me', 'Me', 'Same. Launch first, then the open bits.'],
        ['02:07', 'lena', 'Lena', 'Morning! Are we still on for the spring launch on the 14th?'],
        ['02:09', 'me', 'Me', 'Yes, as long as the onboarding screens are ready.'],
        ['02:12', 'sam', 'Sam', "They're close. I'll have them done by Thursday."],
        ['02:14', 'me', 'Me', 'Great. Then we keep the 14th and skip the extra beta week.'],
        ['02:16', 'lena', 'Lena', "I'll draft the release notes by Friday."],
        ['02:19', 'sam', 'Sam', 'One open thing: do we still need a new pricing page?'],
        ['02:22', 'lena', 'Lena', "Let's park that until next week."],
      ],
    },
    sam: {
      title: '1:1 with Sam', when: 'Yesterday, 16:30', lines: [
        ['12:01', 'me', 'Me', 'How are the onboarding screens looking?'],
        ['12:05', 'sam', 'Sam', "Close. They're the only thing that could push the launch."],
        ['12:08', 'me', 'Me', "Then let's protect the 14th and drop something else."],
        ['12:11', 'sam', 'Sam', 'We could skip the extra beta week. Hardly anyone signed up for it.'],
        ['12:14', 'me', 'Me', "Good idea. Let's raise it in the sync tomorrow."],
        ['12:17', 'sam', 'Sam', 'And I can demo the screens on Thursday.'],
      ],
    },
  };
  // hits: "*" = found by both keywords and meaning, "~" = too weak to count
  const QA = [
    {
      q: 'What did we decide about the launch date?',
      terms: ['launch', 'date'], kw: ['Spring sync 02:07', 'Spring sync 02:14'],
      sem: ['Spring sync 02:14', '1:1 Sam 12:11', 'Spring sync 02:12'],
      fused: ['Spring sync 02:14*', '1:1 Sam 12:11', 'Spring sync 02:12'],
      found: 'Found 3 passages in 2 meetings', ms: 24,
      answer: ['The spring launch stays on the 14th, and you skip the extra beta week to keep it', ['spring', '02:14'], '. Sam floated that trade in your 1:1 the day before', ['sam', '12:11'], '. The one risk: the onboarding screens, due Thursday', ['spring', '02:12'], '.'],
      think: 'Keywords and meaning, woven together…', say: 'Here\'s the thread, every bit of it sourced.', open: '02:14, the exact moment. Have a listen.',
    },
    {
      q: 'Who owns the release notes?',
      terms: ['release', 'notes'], kw: ['Spring sync 02:16'],
      sem: ['Spring sync 02:16', 'Spring sync 02:12'],
      fused: ['Spring sync 02:16*', 'Spring sync 02:12'],
      found: 'Found 2 passages in 1 meeting', ms: 21,
      answer: ['Lena does. She said she\'d draft the release notes by Friday', ['spring', '02:16'], '. They wait on the onboarding screens, which Sam is finishing by Thursday', ['spring', '02:12'], '.'],
      think: 'Release notes… following that thread.', say: 'Lena has it, due Friday.', open: '02:16, in Lena\'s own words.',
    },
    {
      q: 'Did anyone mention the budget?',
      terms: ['budget'], kw: [],
      sem: ['Spring sync 02:19~'],
      fused: [],
      found: 'Nothing about this in 42 meetings', ms: 23, none: true,
      answer: ['Nobody mentioned the budget, at least not while Mostalgia was listening. I searched all 42 meetings and found nothing to cite, so I won\'t guess.'],
      think: 'Budget, budget… hmm.', say: 'If nobody said it, I\'ll tell you so. No confident inventions.',
    },
  ];
  const SUGGEST_LINE = 'Lena\'s asking you. Here\'s your answer, with proof.';
  const START_LINE = 'Mid-meeting and lost the thread? Ask me anything.';

  // ───── elements ─────
  const chat = $('#e5Chat'), drawer = $('#e5Drawer'), list = $('.e5-lines', drawer);
  const player = $('.e5-player', drawer), wave = $('.e5-wave', drawer), pt = $('.e5-pt', drawer);
  const chips = $$('.e5-q', scene), field = $('.e5-field', scene), send = $('.e5-send', scene);
  const sayTxt = $('.e5-say-txt', scene), tsumugi = $('.e5-tsumugi', scene), suggest = $('.e5-suggest', scene);
  const pointer = $('.e5-pointer', scene), clocks = $$('.e5-clock', scene), navItems = $$('.e5-item[data-m]', scene);
  const replay = $('#e5Replay'), status = $('#e5Status');
  const placeholder = field.textContent;
  scene.classList.add('e5-js');

  const el = (tag, cls, text) => Object.assign(document.createElement(tag), cls ? { className: cls } : {}, text != null ? { textContent: text } : {});
  const cite = (m, t) => {
    const b = el('button', 'e5-cite', `${MEETINGS[m].title} · ${t}`);
    b.type = 'button';
    b.dataset.m = m;
    b.dataset.t = t;
    b.setAttribute('aria-label', `Open ${MEETINGS[m].title} at ${t}`);
    return b;
  };
  const plain = (qa) => qa.answer.map((x) => (typeof x === 'string' ? x : ` (${MEETINGS[x[0]].title}, ${x[1]})`)).join('');

  // the waveform under the transcript
  for (let i = 0; i < 34; i++) {
    const bar = el('i');
    bar.style.cssText = `--i:${i};--h:${Math.round(22 + 70 * Math.abs(Math.sin(i * 1.7) * Math.cos(i * 0.45)))}%`;
    wave.append(bar);
  }

  // ───── clock: only runs while the scene is on screen ─────
  let userOpened = false, pending = [], t = 0, last = 0, raf = 0, visible = false, started = false, touched = false, recT = 341, shownRec = '';
  const at = (time, fn) => pending.push({ time, fn });
  const kick = () => { if (!raf && visible && !reduce) { last = performance.now(); raf = requestAnimationFrame(frame); } };
  const later = (dt, fn) => { if (reduce) fn(); else { at(t + dt, fn); kick(); } };
  const mmss = (s) => `${String(Math.floor(s / 60)).padStart(2, '0')}:${String(Math.floor(s % 60)).padStart(2, '0')}`;
  function frame(now) {
    raf = 0;
    const dt = Math.min(0.25, (now - last) / 1000); // tolerate slow frames, skip long stalls
    last = now;
    t += dt;
    recT += dt;
    const due = pending.filter((e) => e.time <= t).sort((a, b) => a.time - b.time);
    pending = pending.filter((e) => e.time > t);
    due.forEach((e) => e.fn());
    const c = mmss(recT);
    if (c !== shownRec) { shownRec = c; clocks.forEach((k) => { k.textContent = c; }); }
    if (visible) raf = requestAnimationFrame(frame);
  }

  // ───── Tsumugi ─────
  const pose = (p, line) => {
    const src = sprite('tsumugi', p);
    if (tsumugi.getAttribute('src') !== src) {
      tsumugi.src = src;
      if (!reduce) { tsumugi.classList.remove('hop'); void tsumugi.offsetWidth; tsumugi.classList.add('hop'); }
    }
    if (line) type(sayTxt, line);
  };

  // ───── transcript drawer ─────
  const fillDrawer = (m) => {
    if (drawer.dataset.m === m) return;
    const mt = MEETINGS[m];
    drawer.dataset.m = m;
    $('.e5-dr-title', drawer).textContent = mt.title;
    $('.e5-dr-when', drawer).textContent = mt.when;
    list.replaceChildren(...mt.lines.map(([ts, who, name, text]) => {
      const li = el('li');
      li.dataset.t = ts;
      li.append(el('span', 'e5-ts', ts), el('span', `e5-chip ${who}`, name), el('span', '', text));
      return li;
    }));
  };
  let openRun = 0;
  const openAt = (m, ts, play = true) => {
    const run = ++openRun;
    const wasOpen = drawer.classList.contains('open');
    fillDrawer(m);
    $$('.e5-cite', scene).forEach((c) => c.classList.toggle('on', c.dataset.m === m && c.dataset.t === ts));
    navItems.forEach((n) => n.classList.toggle('on', n.dataset.m === m));
    drawer.classList.add('open');
    scene.classList.add('e5-dr-open');
    // on phones the transcript is a sheet over the chat: lift the clicked source above it
    const on = $('.e5-ans .e5-cite.on', chat);
    if (on && getComputedStyle(drawer).position === 'absolute') {
      chat.scrollBy({ top: on.getBoundingClientRect().top - chat.getBoundingClientRect().top - 70, behavior: reduce ? 'auto' : 'smooth' });
    }
    const lines = $$('li', list);
    const line = lines.find((l) => l.dataset.t === ts);
    lines.forEach((l) => l.classList.remove('hit', 'flash'));
    pt.textContent = ts;
    player.classList.remove('playing');
    // wait for the drawer to finish sliding (its width changes how lines wrap), then scroll to the line
    later(wasOpen ? 0.05 : 0.5, () => {
      if (run !== openRun) return;
      list.scrollTo({ top: line.offsetTop - (list.clientHeight - line.offsetHeight) / 2, behavior: reduce ? 'auto' : 'smooth' });
      later(0.35, () => {
        if (run !== openRun) return;
        line.classList.add('hit', 'flash');
        if (!play || reduce) return;
        void player.offsetWidth;
        player.classList.add('playing');
        later(3.4, () => { if (run === openRun) player.classList.remove('playing'); });
      });
    });
  };
  const closeDrawer = () => {
    openRun++;
    drawer.classList.remove('open');
    scene.classList.remove('e5-dr-open');
    player.classList.remove('playing');
    navItems.forEach((n) => n.classList.remove('on'));
    $$('.e5-cite', scene).forEach((c) => c.classList.remove('on'));
  };

  // ───── one question and its answer ─────
  const hitsHTML = (hits) => (hits.length
    ? hits.map((h) => `<span class="e5-hit${h.endsWith('*') ? ' both' : ''}${h.endsWith('~') ? ' weak' : ''}">${h.replace(/[*~]$/, '')}</span>`).join('')
    : '<span class="e5-hit nil">no match</span>');
  const build = (qa) => {
    const you = el('p', 'e5-you', qa.q);
    const find = el('div', 'e5-find');
    find.innerHTML = `<p class="e5-find-head"><span class="e5-find-txt">Searching 42 meetings…</span> <span class="e5-ms">${qa.ms} ms · all local</span></p>`
      + '<div class="e5-fb"><div>'
      + `<div class="e5-lane kw"><b>Keywords</b>${qa.terms.map((w) => `<em>“${w}”</em>`).join(' ')} ${hitsHTML(qa.kw)}</div>`
      + `<div class="e5-lane sem"><b>Meaning</b>${hitsHTML(qa.sem)}</div>`
      + `<div class="e5-lane e5-fused"><b>Fused</b>${qa.fused.length ? hitsHTML(qa.fused) : '<span class="e5-hit nil">nothing strong enough</span>'}</div>`
      + '</div></div>';
    const ans = el('div', 'e5-ans');
    const p = el('p');
    const parts = [];
    qa.answer.forEach((tok) => {
      if (typeof tok !== 'string') {
        const b = cite(tok[0], tok[1]);
        p.append(' ', b);
        parts.push(b);
        return;
      }
      tok.split(/(\s+)/).forEach((w) => {
        if (!w) return;
        if (/^\s+$/.test(w)) { p.append(w); return; }
        const s = el('span', 'e5-w', w);
        p.append(s);
        parts.push(s);
      });
    });
    ans.append(p);
    if (qa.none) {
      const n = el('span', 'e5-none', 'Sources · none, so nothing is cited');
      ans.append(n);
      parts.push(n);
    }
    return { you, find, ans, parts };
  };
  const firstCite = (qa) => qa.answer.find((x) => typeof x !== 'string');

  const setChip = (i) => chips.forEach((c, k) => c.setAttribute('aria-pressed', String(k === i)));
  const tap = (node) => { node.classList.add('tap'); later(0.16, () => node.classList.remove('tap')); };
  const pointAt = (node, dx = 0.5, dy = 0.55) => {
    const r = node.getBoundingClientRect(), s = scene.getBoundingClientRect();
    pointer.style.transform = `translate(${r.left - s.left + r.width * dx}px, ${r.top - s.top + r.height * dy}px)`;
  };
  const toBottom = () => chat.scrollTo({ top: chat.scrollHeight, behavior: reduce ? 'auto' : 'smooth' });

  // show a question answered at once (reduced motion)
  const showNow = (i) => {
    const qa = QA[i];
    const { you, find, ans } = build(qa);
    find.classList.add('done');
    if (qa.none) find.classList.add('none');
    $('.e5-find-txt', find).textContent = qa.found;
    chat.replaceChildren(you, find, ans);
    const c = firstCite(qa);
    if (c) openAt(c[0], c[1], false); else closeDrawer();
    pose(qa.none ? 'idle' : 'talking', qa.say);
    status.textContent = plain(qa);
  };

  // play a question out: typed in, searched, answered word by word, then opened at its first source
  const run = (i, { auto = false } = {}) => {
    const qa = QA[i];
    setChip(i);
    if (reduce) { showNow(i); return; }
    pending = [];
    t = 0;
    userOpened = false;
    replay.hidden = true;
    closeDrawer();
    field.textContent = placeholder;
    field.classList.remove('typing');
    chat.replaceChildren();
    status.textContent = '';
    let T = 0.15;
    if (auto) {
      suggest.classList.remove('in');
      chat.append(el('p', 'e5-empty', 'Ask anything about your 42 meetings.'));
      $('.e5-empty', chat).prepend(el('span', '', '✦'));
      pose('listening', START_LINE);
      // the pointer drifts in from the corner and clicks the first suggestion
      pointer.style.transition = 'none';
      pointAt(scene, 0.62, 0.95);
      void pointer.offsetWidth;
      pointer.style.transition = '';
      at(1.4, () => { pointer.classList.add('show'); pointAt(chips[i], 0.3); });
      at(2.25, () => { pointer.classList.remove('tap'); void pointer.offsetWidth; pointer.classList.add('tap'); tap(chips[i]); });
      at(2.9, () => pointer.classList.remove('show'));
      T = 2.5;
    }
    at(T, () => { chat.replaceChildren(); field.classList.add('typing'); field.textContent = ''; });
    [...qa.q].forEach((_, k) => at(T + 0.05 + k * 0.03, () => { field.textContent = qa.q.slice(0, k + 1); }));
    T += 0.05 + qa.q.length * 0.03 + 0.3;
    at(T, () => send.classList.add('press'));
    const { you, find, ans, parts } = build(qa);
    at(T + 0.18, () => {
      send.classList.remove('press');
      field.classList.remove('typing');
      field.textContent = placeholder;
      chat.append(you);
    });

    // hybrid search: keyword hits, meaning hits, then one fused ranking
    const S = T + 0.5;
    const head = $('.e5-find-txt', find);
    at(S, () => { chat.append(find); toBottom(); pose('thinking', qa.think); });
    $$('.kw .e5-hit', find).forEach((h, k) => at(S + 0.3 + k * 0.17, () => h.classList.add('in')));
    $$('.sem .e5-hit', find).forEach((h, k) => at(S + 0.5 + k * 0.17, () => h.classList.add('in')));
    at(S + 1.3, () => { find.classList.add('fuse'); head.textContent = 'Fusing keyword and meaning ranks…'; toBottom(); });
    $$('.e5-fused .e5-hit', find).forEach((h, k) => at(S + 1.4 + k * 0.15, () => h.classList.add('in')));
    at(S + 2.6, () => { find.classList.add('done'); if (qa.none) find.classList.add('none'); head.textContent = qa.found; });

    // the answer streams in
    const A = S + 3.0;
    at(A, () => { chat.append(ans); toBottom(); pose(qa.none ? 'idle' : 'talking', qa.say); });
    parts.forEach((part, k) => at(A + 0.1 + k * 0.06, () => part.classList.add('in')));
    let end = A + 0.1 + parts.length * 0.06 + 0.3;
    at(end, () => { toBottom(); if (!auto) status.textContent = plain(qa); });

    // then the first source opens at its line
    const c = firstCite(qa);
    if (c) {
      const chip = $(`.e5-cite[data-m="${c[0]}"][data-t="${c[1]}"]`, ans);
      if (auto) {
        at(end + 0.5, () => { if (!userOpened) { pointer.classList.add('show'); pointAt(chip, 0.25); } });
        at(end + 1.35, () => { if (!userOpened) { pointer.classList.remove('tap'); void pointer.offsetWidth; pointer.classList.add('tap'); tap(chip); } });
        end += 1.4;
      } else end += 0.8;
      at(end, () => { if (!userOpened) { openAt(c[0], c[1]); pose('writing', qa.open); } });
      at(end + 0.9, () => pointer.classList.remove('show'));
      end += 3.4;
      // mid-meeting, someone asks you something: a suggested answer, with proof
      if (i === 0 && !suggest.classList.contains('in')) {
        at(end, () => { suggest.classList.add('in'); pose('talking', SUGGEST_LINE); });
        end += 2.2;
      }
    }
    at(end, () => { replay.hidden = false; });
    kick();
  };

  // ───── wiring ─────
  chips.forEach((c, i) => c.addEventListener('click', () => {
    touched = started = true;
    pointer.classList.remove('show');
    run(i);
  }));
  scene.addEventListener('click', (e) => {
    const c = e.target.closest('.e5-cite');
    if (c) {
      userOpened = true;
      pointer.classList.remove('show');
      openAt(c.dataset.m, c.dataset.t);
      return;
    }
    if (e.target.closest('.e5-dr-x')) closeDrawer();
  });
  replay.addEventListener('click', () => { touched = started = true; run(0, { auto: true }); });

  ['listening', 'thinking', 'talking', 'writing', 'idle'].forEach((p) => preload(sprite('tsumugi', p)));

  if (reduce) {
    // the finished first answer is already in the HTML; just make the transcript complete
    openAt('spring', '02:14', false);
    return;
  }

  scene.classList.add('e5-anim');
  // rewind to an empty Ask view until the scene comes into view
  setChip(-1);
  closeDrawer();
  chat.replaceChildren(el('p', 'e5-empty', 'Ask anything about your 42 meetings.'));
  $('.e5-empty', chat).prepend(el('span', '', '✦'));
  sayTxt.textContent = START_LINE;
  tsumugi.src = sprite('tsumugi', 'listening');

  // start once ~40% of the scene (or of the screen, on tall phones) is in view; pause whenever it leaves
  new IntersectionObserver(([e]) => {
    const vh = e.rootBounds ? e.rootBounds.height : innerHeight;
    const seen = e.intersectionRect.height / Math.min(e.boundingClientRect.height, vh);
    visible = started ? e.isIntersecting : seen >= 0.4;
    scene.classList.toggle('e5-paused', !visible);
    if (visible && !started) { started = true; if (!touched) run(0, { auto: true }); }
    if (visible) kick(); else { cancelAnimationFrame(raf); raf = 0; }
  }, { threshold: [0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1] }).observe(scene);
})();
