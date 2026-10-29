// Episode 7: the cast select. Five companions take turns on one stage; the mini
// screen beside them plays the jobs they do. The HTML already holds every profile
// and the finished frame of the first job, so it reads fine without JS.
(() => {
  const S = window.MostalgiaSite;
  const root = document.getElementById('castScene');
  if (!S || !root) return;
  const { reduce, $, $$, sprite, type, preload } = S;

  // how each one shows notes, how they answer, and what they say about each job
  const CAST = {
    mori: {
      kana: 'モリ', role: 'The note-taker', as: 'notepad', honest: 'local',
      say: {
        notes: "That's a decision. Pinned, with the owner.",
        desk: "I'll sit on your call window. Quietly.",
        regen: 'Want it shorter? Let me try that again.',
        honest: "A local model is answering. I'll say so if that changes.",
        calm: "Paused? Then I'm resting my eyes.",
        off: "I'm off until you switch me on.",
      },
    },
    tsumugi: {
      kana: 'ツムギ', role: 'The weaver', as: 'scarf', prop: 'tsumugi-notes', honest: 'cloud',
      say: {
        notes: 'One more thread for the scarf: Lena, release notes, Friday.',
        desk: "I'll wait on the edge of your screen. Take your time.",
        regen: "Let me unpick this and weave it again, around the deadlines.",
        honest: "That answer came from your cloud model. I'll always say which.",
        calm: "The meeting's paused… I'll just close my eyes a moment.",
        off: "Only if you'd like me. I'm off by default.",
      },
    },
    hoshi: {
      kana: 'ホシ', role: 'The astronomer', as: 'star chart', prop: 'hoshi-notes', honest: 'none',
      say: {
        notes: 'A new star: an open question. It links to the pricing page.',
        desk: 'From up here I can see every monitor. Fascinating.',
        regen: 'Re-plotting with a new focus. The old chart is kept.',
        honest: "No model is set up yet, so I won't pretend to know.",
        calm: 'Paused. Even stars need a rest.',
        off: "Switch me on in Settings, whenever you're curious.",
      },
    },
    kaito: {
      kana: 'カイト', role: 'The tea host', as: 'cups of tea', prop: 'kaito-notes', honest: 'fail',
      say: {
        notes: 'One decision, freshly poured. Shall I pin it?',
        desk: "Please, carry on with your call. I'll wait up here.",
        regen: "Not to your taste? I'll brew another pot.",
        honest: 'My apologies: the provider timed out. No made-up answer.',
        calm: "A short pause. Allow me a moment's rest.",
        off: 'Only at your invitation: Settings, Companion.',
      },
    },
    jin: {
      kana: 'ジン', role: 'The wandering swordsman', as: 'paper strips', prop: 'jin-notes', honest: 'local',
      say: {
        notes: 'Decision. Owner. Due date. Done.',
        desk: 'On top of the call. Out of the way.',
        regen: 'Again. Sharper.',
        honest: 'Local model. No pretending.',
        calm: 'Paused. Resting.',
        off: 'Off by default. Your call.',
      },
    },
  };
  const HONEST = {
    local: ['Local model', 'Answering on this computer.'],
    cloud: ['Cloud model', 'Answered by your cloud provider.'],
    none: ['No model', "No model set up yet. I won't guess."],
    fail: ['Provider failed', 'Timed out. No answer, rather than a made-up one.'],
  };
  const JOBS = { notes: 'Live notes', desk: 'On your desktop', regen: 'Regenerate', honest: 'Honest status', calm: 'Alive, but calm', off: 'Entirely optional' };
  const BEAT = 4.6; // seconds per job during the opening round
  const name = (c) => c[0].toUpperCase() + c.slice(1);

  const tiles = $$('.cs-tile', root), chips = $$('.cs-chip', root), vigs = $$('.cs-v', root);
  const big = $('#csBig'), plate = $('#csPlate'), kana = $('#csKana'), lines = $('.cs-lines', root);
  const bubble = $('#csSay'), who = $('#csWho'), line = $('#csLine');
  const capWho = $('#csCapWho'), cap = $('#csCap'), replay = $('#csReplay');
  const vig = Object.fromEntries(vigs.map((v) => [v.dataset.f, v]));
  const hi = Object.fromEntries(tiles.map((t) => [t.dataset.c, $(`#cs-p-${t.dataset.c} .cs-quote`).textContent.replace(/[“”]/g, '')]));

  let cur = 'mori', job = 'notes';

  // ── the pieces every beat is made of ──
  const restart = (el, cls) => { el.classList.remove(cls); void el.offsetWidth; el.classList.add(cls); };
  const mini = (f, p) => {
    const img = $('.cs-mini', vig[f]), src = sprite(cur, p);
    if (img.getAttribute('src') !== src) { img.src = src; if (!reduce && vig[f].classList.contains('on')) restart(img, 'hop'); }
  };
  const pose = (p, f) => {
    const src = sprite(cur, p);
    if (big.getAttribute('src') !== src) { big.src = src; if (!reduce && !big.classList.contains('in')) restart(big, 'hop'); }
    if (f) mini(f, p);
  };
  const say = (text) => {
    who.textContent = name(cur);
    type(line, text);
    if (!reduce) restart(bubble, 'pop');
  };
  const select = (c, focus) => {
    cur = c;
    tiles.forEach((t) => {
      const on = t.dataset.c === c;
      t.setAttribute('aria-selected', String(on));
      t.tabIndex = on ? 0 : -1;
      if (on && focus) t.focus();
    });
    $$('.cs-prof', root).forEach((p) => { p.hidden = p.id !== `cs-p-${c}`; });
    const d = CAST[c];
    plate.innerHTML = `<b>${name(c)}</b><small>${d.role}</small>`;
    kana.textContent = d.kana;
    capWho.textContent = name(c);
    // how this one shows notes, and how they answer
    const as = $('.cs-as', vig.notes);
    if (d.prop) { as.dataset.prop = d.prop; $('img', as).src = sprite(c, 'notes'); } else delete as.dataset.prop;
    $('em', as).textContent = `as: ${d.as}`;
    big.classList.remove('hop');
    big.src = sprite(c, 'idle');
    if (!reduce) { restart(big, 'in'); restart(plate, 'in'); restart(kana, 'in'); restart(lines, 'flash'); }
    // warm up this companion's poses
    ['listening', 'talking', 'thinking', 'writing', 'sleeping', 'error'].forEach((p) => preload(sprite(c, p)));
  };
  big.addEventListener('animationend', (e) => { if (e.animationName === 'cs-in') big.classList.remove('in'); });

  // ── one job, played by the current companion (times in seconds from t0) ──
  const job$ = (at, t0, f) => {
    const d = () => CAST[cur]; // read when the beat plays, not when it is scheduled
    at(t0, () => {
      job = f;
      chips.forEach((b) => {
        const on = b.dataset.f === f;
        b.setAttribute('aria-pressed', String(on));
        b.classList.toggle('run', false);
      });
      cap.textContent = JOBS[f];
      vigs.forEach((v) => v.classList.toggle('on', v === vig[f]));
      if (!reduce) { restart(vig[f], 'on'); restart($(`.cs-chip[data-f="${f}"]`, root), 'run'); }
      say(d().say[f]);
    });
    const v = vig[f];
    if (f === 'notes') at(t0, () => pose('writing', f));
    if (f === 'desk') {
      at(t0, () => pose('listening', f));
      at(t0 + 2.7, () => pose('idle', f));
    }
    if (f === 'regen') {
      at(t0, () => pose('thinking', f));
      at(t0 + 1.55, () => pose('writing', f));
    }
    if (f === 'honest') {
      const reply = $('.cs-reply', v);
      at(t0, () => {
        const h = d().honest;
        reply.className = `cs-reply ${h}`;
        $('.cs-badge', v).textContent = HONEST[h][0];
        $('#csStatus').textContent = '';
        pose(h === 'none' ? 'thinking' : 'listening', f);
      });
      at(t0 + 1.0, () => {
        const h = d().honest;
        type($('#csStatus'), HONEST[h][1]);
        pose({ fail: 'error', none: 'thinking' }[h] || 'talking', f);
      });
    }
    if (f === 'calm') {
      const tag = $('.cs-pose', v);
      const step = (p, label) => () => { pose(p, f); tag.textContent = label; if (!reduce) restart(tag, 'pop'); };
      at(t0, () => { v.classList.add('cs-live'); $('.cs-st', v).textContent = 'Listening'; });
      [['idle', 'idle'], ['listening', 'listening'], ['writing', 'writing'], ['thinking', 'thinking'], ['talking', 'talking']]
        .forEach(([p, l], i) => at(t0 + i * 0.5, step(p, l)));
      at(t0 + 2.6, () => { v.classList.remove('cs-live'); $('.cs-st', v).textContent = 'Paused'; step('sleeping', 'dozing')(); });
    }
    if (f === 'off') {
      at(t0, () => pose('idle', f));
      at(t0 + 1.5, () => pose('talking', f));
    }
  };

  // ── a tiny clock that only runs while the scene is on screen ──
  let events = [], next = 0, t = 0, last = 0, raf = 0, visible = false, started = false;
  const frame = (now) => {
    raf = 0;
    t += Math.min(0.5, (now - last) / 1000); // generous cap: slow machines keep time, a stalled tab does not leap
    last = now;
    while (next < events.length && events[next].time <= t) events[next++].fn();
    if (visible && next < events.length) raf = requestAnimationFrame(frame);
  };
  const play = () => {
    root.classList.remove('cs-paused');
    if (!raf && next < events.length) { last = performance.now(); raf = requestAnimationFrame(frame); }
  };
  const pause = () => { cancelAnimationFrame(raf); raf = 0; root.classList.add('cs-paused'); };
  const run = (build) => {
    cancelAnimationFrame(raf); raf = 0;
    const ev = [];
    build((time, fn) => ev.push({ time, fn }));
    events = ev.sort((a, b) => a.time - b.time);
    next = 0; t = 0;
    if (reduce) { events.forEach((e) => e.fn()); next = events.length; return; }
    if (visible) play();
  };

  // the opening round: everyone steps up once, each doing a different job
  const ROUND = [['mori', 'notes'], ['tsumugi', 'desk'], ['hoshi', 'regen'], ['kaito', 'honest'], ['jin', 'calm'], ['mori', 'off']];
  const intro = () => run((at) => {
    replay.hidden = true;
    ROUND.forEach(([c, f], i) => {
      at(i * BEAT, () => select(c));
      job$(at, i * BEAT + 0.05, f);
    });
    const end = ROUND.length * BEAT;
    at(end - 0.6, () => { pose('idle'); say('Now, your pick. Who takes your notes?'); });
    at(end, () => { replay.hidden = false; chips.forEach((b) => b.classList.remove('run')); });
  });

  // the visitor picks: the companion hops in, greets, then shows the current job their way
  const pick = (c, focus) => {
    run((at) => {
      at(0, () => {
        select(c, focus);
        say(hi[c]);
        setTimeout(() => pose('talking'), reduce ? 0 : 450);
        vigs.forEach((v) => $('.cs-mini', v) && mini(v.dataset.f, v.dataset.f === 'calm' ? 'sleeping' : 'idle'));
      });
      job$(at, reduce ? 0 : 2.6, job);
      if (reduce) at(0, () => say(hi[c]));
    });
    replay.hidden = false;
  };
  tiles.forEach((tile) => tile.addEventListener('click', () => pick(tile.dataset.c)));
  $('.cs-roster', root).addEventListener('keydown', (e) => {
    const i = tiles.findIndex((tl) => tl.dataset.c === cur);
    const to = { ArrowRight: i + 1, ArrowDown: i + 1, ArrowLeft: i - 1, ArrowUp: i - 1, Home: 0, End: tiles.length - 1 }[e.key];
    if (to === undefined) return;
    e.preventDefault();
    pick(tiles[(to + tiles.length) % tiles.length].dataset.c, true);
  });
  chips.forEach((b) => b.addEventListener('click', () => {
    run((at) => job$(at, 0, b.dataset.f));
    replay.hidden = false;
  }));
  replay.addEventListener('click', () => { pause(); started = visible = true; intro(); play(); });

  // ── set the stage ──
  root.classList.add('cs-js');
  if (!reduce) root.classList.add('cs-anim');
  select('mori');
  big.classList.remove('in', 'hop');
  if (reduce) {
    // the finished frame, still selectable
    run((at) => { job$(at, 0, 'notes'); at(0, () => say(hi.mori)); });
    replay.remove();
    return;
  }
  say(hi.mori);
  ['writing', 'listening', 'thinking', 'error', 'sleeping', 'talking'].forEach((p) => preload(sprite('mori', p)));
  [['tsumugi', 'listening'], ['hoshi', 'thinking'], ['kaito', 'error'], ['jin', 'sleeping']].forEach(([c, p]) => preload(sprite(c, p)));

  // start once ~40% of the stage (or of the screen, on tall phones) is in view; pause whenever it leaves
  new IntersectionObserver(([e]) => {
    const vh = e.rootBounds ? e.rootBounds.height : innerHeight;
    const seen = e.intersectionRect.height / Math.min(e.boundingClientRect.height, vh);
    if (!started) {
      if (seen < 0.4) return;
      started = visible = true;
      intro();
      return;
    }
    visible = e.isIntersecting;
    if (visible) play(); else pause();
  }, { threshold: [0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1] }).observe(root);
})();
