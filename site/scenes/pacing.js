// Shared pacing for the episode scenes: chapters with a caption, a slower
// clock with holds between key moments, a soft spotlight on what matters
// right now, and visitor controls (pause/play, jump to a chapter, replay).
//
// A scene keeps writing its beats in its own "scene seconds" and hands them
// over here instead of running its own clock:
//
//   MostalgiaSite.timeline(sceneEl, chapters, {
//     bar,        // the <ul>/<ol> under the scene: its <li>s become the chapter buttons (one per chapter)
//     ctlBefore,  // the caption + pause row is inserted before this element (default: sceneEl)
//     watch,      // element whose visibility starts/pauses playback (default: sceneEl)
//     parts,      // selector list of the pieces the spotlight may dim
//     replay,     // the scene's Replay button (shown at the end)
//     end,        // scene seconds at which the scene is finished
//     reset(),    // rewind the DOM; return the beats as [{ time, fn(late) }]
//     tick(t, dt) // optional per-frame update (t: scene seconds, dt: real seconds, also during holds)
//     slow, hold, // playback stretch (1.5 = 1.5x longer) and the default hold after each chapter (s)
//     sticky,     // keep the caption row stuck to the top of the screen on phones
//   });
//
// A chapter is { id, label, caption, start, focus: [selectors], holds?: [t | [t, seconds]], hold? }.
// It runs from its start to the next chapter's start (the last one to `end`).
// `holds` are extra pauses inside a chapter; `hold` overrides the pause after it.
// Beats fire once the clock passes them; `late` says by how much (large when
// a chapter jump fast-forwards through earlier beats, so a scene can skip
// pure flourishes then). CSS animations, transitions and Web Animations inside
// the scene are slowed by the same factor, paused with the clock and
// finished instantly on a jump, so jumps land on the right state.
(() => {
  const site = (window.MostalgiaSite = window.MostalgiaSite || {});
  const reduce = site.reduce ?? matchMedia('(prefers-reduced-motion: reduce)').matches;
  const PACE = 'pace'; // id of the spotlight's own Web Animations, which the clock leaves alone
  const FADE = 650;

  site.timeline = (scene, chapters, o) => {
    const slow = o.slow || 1.5;
    const holdDefault = o.hold ?? 1.6;
    const dimTo = o.dim || 'opacity(.5) saturate(.45)';
    const n = chapters.length;

    // ───── paced time: scene seconds stretched by `slow`, with holds where the scene clock stands still ─────
    const segs = [], startP = [];
    let total = 0;
    const run = (a, b) => { if (b > a) { segs.push({ p0: total, p1: total + (b - a) * slow, s0: a, s1: b }); total += (b - a) * slow; } };
    const halt = (s, d) => { if (d > 0) { segs.push({ p0: total, p1: total + d, s0: s, s1: s }); total += d; } };
    chapters.forEach((c, i) => {
      c.end = i + 1 < n ? chapters[i + 1].start : o.end;
      startP[i] = total;
      let s = c.start;
      (c.holds || []).map((h) => (Array.isArray(h) ? h : [h, holdDefault * 0.75])).sort((a, b) => a[0] - b[0]).forEach(([h, d]) => { run(s, h); halt(h, d); s = h; });
      run(s, c.end);
      halt(c.end, c.hold ?? (i + 1 < n ? holdDefault : 0));
    });
    const sceneAt = (p) => {
      for (const g of segs) if (p < g.p1) return g.p1 > g.p0 ? g.s0 + ((g.s1 - g.s0) * (p - g.p0)) / (g.p1 - g.p0) : g.s0;
      return o.end;
    };
    const chapterAt = (p) => { let i = 0; while (i + 1 < n && startP[i + 1] <= p) i += 1; return i; };

    // ───── caption row + chapter bar ─────
    const ctl = document.createElement('div');
    ctl.className = `pace-ctl${o.sticky ? ' pace-sticky' : ''}`;
    const playBtn = document.createElement('button');
    playBtn.type = 'button';
    playBtn.className = 'pace-play';
    const cap = document.createElement('p');
    cap.className = 'pace-cap';
    cap.setAttribute('aria-live', 'polite');
    const capNo = document.createElement('span'), capTxt = document.createElement('span');
    capNo.className = 'pace-capno';
    capTxt.className = 'pace-captxt';
    cap.append(capNo, capTxt);
    if (!reduce) ctl.append(playBtn);
    ctl.append(cap);
    const before = o.ctlBefore || scene;
    before.parentNode.insertBefore(ctl, before);

    const bar = o.bar;
    bar.classList.add('pace-bar', 'pace-on');
    if (!bar.getAttribute('aria-label')) bar.setAttribute('aria-label', 'Chapters');
    const items = [...bar.children];
    const btns = chapters.map((c, i) => {
      const li = items[i] || bar.appendChild(document.createElement('li'));
      const b = document.createElement('button');
      b.type = 'button';
      b.className = 'pace-chap';
      const num = document.createElement('span');
      num.className = 'pace-n';
      num.setAttribute('aria-hidden', 'true');
      num.textContent = i + 1;
      const txt = document.createElement('span');
      txt.className = 'pace-t';
      if (li.childNodes.length) txt.append(...li.childNodes);
      else txt.innerHTML = `<b>${c.label}</b>`;
      const prog = document.createElement('i');
      prog.className = 'pace-prog';
      prog.setAttribute('aria-hidden', 'true');
      b.append(num, txt, prog);
      b.setAttribute('aria-label', `Chapter ${i + 1} of ${n}: ${c.label}`);
      li.append(b);
      b.addEventListener('click', () => jump(i));
      return b;
    });

    // ───── spotlight: dim the parts that the current chapter is not about ─────
    const dims = new Map(); // element -> its dimming animation
    const parts = o.parts ? [...scene.querySelectorAll(o.parts)] : [];
    const dimEl = (el, instant) => {
      if (dims.has(el)) return;
      const a = el.animate([{ filter: dimTo }], { duration: instant ? 0 : FADE, easing: 'ease', fill: 'forwards', id: PACE });
      dims.set(el, a);
    };
    const undimEl = (el, instant) => {
      const a = dims.get(el);
      if (!a) return;
      dims.delete(el);
      const from = getComputedStyle(el).filter;
      a.cancel();
      if (!instant && from && from !== 'none') el.animate([{ filter: from }, {}], { duration: FADE, easing: 'ease', id: PACE });
    };
    const spotlight = (i, instant) => {
      const focus = i == null ? null : (chapters[i].focus || []).flatMap((sel) => [...scene.querySelectorAll(sel)]);
      const out = new Set(focus ? parts.filter((p) => !focus.some((f) => p.contains(f) || f.contains(p))) : []);
      // dim only the outermost piece, so nested parts never dim twice
      for (const p of [...out]) if (parts.some((q) => q !== p && out.has(q) && q.contains(p))) out.delete(p);
      [...dims.keys()].forEach((el) => { if (!out.has(el)) undimEl(el, instant); });
      out.forEach((el) => dimEl(el, instant));
    };

    // ───── the clock ─────
    let events = [], next = 0, P = 0, t = 0, cur = -1, raf = 0, last = 0;
    let started = false, visible = false, userPaused = false, ended = false, running = false;
    const held = new Set();
    const anims = () => scene.getAnimations({ subtree: true }).filter((a) => a.id !== PACE);
    const scaleAnims = () => { const r = 1 / slow; anims().forEach((a) => { if (a.playbackRate !== r) a.playbackRate = r; }); };
    const finishAnims = () => anims().forEach((a) => {
      const it = a.effect && a.effect.getComputedTiming ? a.effect.getComputedTiming().iterations : 1;
      if (it !== Infinity) try { a.finish(); } catch { /* not finishable */ }
    });

    const resume = () => { held.forEach((a) => { if (a.playState === 'paused') a.play(); }); held.clear(); };
    const setChapter = (i) => {
      if (i === cur) return;
      cur = i;
      btns.forEach((b, j) => {
        if (j === i) b.setAttribute('aria-current', 'step'); else b.removeAttribute('aria-current');
        b.classList.toggle('pace-done', j < i);
        if (j !== i) b.style.setProperty('--p', j < i ? 1 : 0);
      });
      capNo.textContent = i == null ? '' : `${i + 1}/${n}`;
      capTxt.textContent = i == null ? (o.intro || 'Pick a chapter to see what happens in it.') : chapters[i].caption;
      if (reduce || (started && !ended)) spotlight(i, reduce);
      if (o.onChapter && i != null) o.onChapter(i);
    };
    const labelPlay = () => {
      const play = userPaused || ended;
      playBtn.innerHTML = play ? '<span class="pace-ic" aria-hidden="true">▶</span><span class="pace-pl">Play</span>' : '<span class="pace-ic" aria-hidden="true">❚❚</span><span class="pace-pl">Pause</span>';
      playBtn.setAttribute('aria-label', ended ? 'Play the scene again from the start' : play ? 'Play the scene' : 'Pause the scene');
      ctl.classList.toggle('pace-paused', userPaused);
    };

    const fire = (upTo, strict) => {
      while (next < events.length && (strict ? events[next].time < upTo : events[next].time <= upTo)) {
        const e = events[next++];
        e.fn(Math.max(0, t - e.time));
      }
    };
    const finish = () => {
      t = o.end;
      fire(o.end, false);
      if (o.tick) o.tick(t, 0);
      ended = true;
      btns[n - 1].style.setProperty('--p', 1);
      btns.forEach((b, j) => b.classList.toggle('pace-done', j < n - 1));
      spotlight(null);
      if (o.replay) o.replay.hidden = false;
      update();
      labelPlay();
    };
    const frame = (now) => {
      raf = 0;
      const dt = Math.min(0.1, (now - last) / 1000);
      last = now;
      P = Math.min(total, P + dt);
      t = sceneAt(P);
      fire(t, true);
      if (o.tick) o.tick(t, dt);
      scaleAnims();
      const i = chapterAt(P);
      setChapter(i);
      const len = (i + 1 < n ? startP[i + 1] : total) - startP[i];
      btns[i].style.setProperty('--p', len > 0 ? ((P - startP[i]) / len).toFixed(4) : 1);
      if (P >= total) { finish(); return; }
      if (running) raf = requestAnimationFrame(frame);
    };
    const update = () => {
      const want = started && visible && !userPaused && !ended;
      if (want === running) return;
      running = want;
      if (want) {
        resume();
        last = performance.now();
        raf = requestAnimationFrame(frame);
      } else {
        cancelAnimationFrame(raf);
        raf = 0;
        // at the end the scene's idle loops keep going, as before
        if (!ended) anims().forEach((a) => { if (a.playState === 'running') { a.pause(); held.add(a); } });
      }
    };

    // rewind and fast-forward to the start of chapter i, with every earlier beat applied
    const seek = (i) => {
      running = false;
      cancelAnimationFrame(raf);
      raf = 0;
      resume();
      ended = false;
      if (o.replay) o.replay.hidden = true;
      events = o.reset().sort((a, b) => a.time - b.time);
      next = 0;
      P = startP[i];
      t = chapters[i].start;
      fire(t, true);
      if (o.tick) o.tick(t, 0);
      finishAnims();
      cur = -1;
      setChapter(i);
      btns[i].style.setProperty('--p', 0);
    };
    // after a jump, bring what the chapter is about into view if it is off screen (tall scenes on phones)
    const reveal = (i) => {
      const el = (chapters[i].focus || []).map((sel) => scene.querySelector(sel)).find(Boolean);
      if (!el) return;
      const r = el.getBoundingClientRect(), top = Math.max(ctl.getBoundingClientRect().bottom, 64);
      if (r.top < top || r.top > innerHeight - 80) {
        scrollBy({ top: r.top - top - 16, behavior: reduce ? 'auto' : 'smooth' });
      }
    };
    const jump = (i) => {
      reveal(i);
      if (reduce) { setChapter(i); return; }
      seek(i);
      started = visible = true;
      userPaused = false;
      spotlight(i);
      update();
      labelPlay();
    };

    if (reduce) {
      cur = -1;
      setChapter(null);
      return { jump };
    }

    playBtn.addEventListener('click', () => {
      if (ended) { jump(0); return; }
      userPaused = !userPaused;
      if (!userPaused) { started = visible = true; }
      update();
      labelPlay();
    });
    if (o.replay) o.replay.addEventListener('click', () => { jump(0); playBtn.focus({ preventScroll: true }); });

    seek(0);
    labelPlay();
    // start once ~40% of the scene (or of the screen, on tall phones) is in view; pause whenever it leaves
    new IntersectionObserver(([e]) => {
      const vh = e.rootBounds ? e.rootBounds.height : innerHeight;
      const seen = e.intersectionRect.height / Math.min(e.boundingClientRect.height, vh);
      visible = started ? e.isIntersecting : seen >= 0.4;
      if (visible && !started) { started = true; spotlight(cur); }
      update();
    }, { threshold: [0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1] }).observe(o.watch || scene);

    return { jump, get total() { return total; }, get sceneTime() { return t; }, get chapter() { return cur; } };
  };
})();
