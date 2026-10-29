// Episode 6: files drop into the Documents drive, get split into passages for
// the index, sort into collections (one switched off), answer a question with a
// meeting moment and a cited page, and finally back up a live answer in a call.
// The HTML holds the finished state; here it is rewound and replayed on a clock
// that only runs while the scene is on screen.
(() => {
  const site = window.MostalgiaSite;
  const scene = document.getElementById('e6Scene');
  if (!site || !scene || site.reduce) return;
  const { $, $$, sprite, type, preload } = site;

  const win = $('.e6-win', scene), list = $('#e6List'), cells = $('#e6Cells');
  const files = $$('.e6-file', scene), cellEls = $$('i', cells);
  const hiring = $('.e6-col[data-col=hiring]', scene), hireSw = $('#e6HireSw');
  const count = $('#e6Count'), looseH = $('.e6-loose-h', scene);
  const cursor = $('#e6Cursor'), input = $('.e6-input', scene), typed = $('#e6Typed'), askBtn = $('#e6AskBtn'), hint = $('.e6-hint', scene);
  const empty = $('.e6-empty', scene), q = $('.e6-q', scene), answer = $('#e6Answer'), srcBits = $$('.e6-sl, .e6-chip', scene), page = $('.e6-page', scene), mark = $('#e6Mark');
  const navAsk = $('.e6-nav-ask', scene), navDocs = $('.e6-nav-docs', scene);
  const live = $('#e6Live'), lvT = $('#e6LvT'), lvLine = $('.e6-lv-line', scene), lvQ = $('#e6LvQ'), card = $('.e6-card', scene);
  const mori = $('#e6Mori'), say = $('#e6Say'), replay = $('#e6Replay');
  const question = q.textContent, liveQ = lvQ.textContent, finalSay = say.textContent;
  const metas = files.map((f) => { const m = $('.e6-meta', f); return { m, done: m.dataset.done, final: m.textContent }; });

  // the answer streams in word by word: wrap each word (and each source number) once
  const words = [];
  const split = (node) => [...node.childNodes].forEach((n) => {
    if (n.nodeType === 1) return n.classList.contains('e6-n') ? (n.classList.add('w'), words.push(n)) : split(n);
    const parts = n.data.split(/(\s+)/);
    n.replaceWith(...parts.map((p) => {
      if (!p.trim()) return document.createTextNode(p);
      const s = document.createElement('span');
      s.className = 'w';
      s.textContent = p;
      words.push(s);
      return s;
    }));
  });
  split(answer);

  const pose = (p, line) => {
    const src = sprite('mori', p);
    if (mori.getAttribute('src') !== src) { mori.src = src; mori.classList.remove('hop'); void mori.offsetWidth; mori.classList.add('hop'); }
    if (line) type(say, line);
  };
  const restart = (el, c) => { el.classList.remove(c); void el.offsetWidth; el.classList.add(c); };

  // the pointer glides to a spot on an element (fractions of its box), relative to the window
  const point = (el, fx = 0.5, fy = 0.5) => {
    const r = el.getBoundingClientRect(), w = win.getBoundingClientRect();
    cursor.style.left = `${r.left - w.left + r.width * fx}px`;
    cursor.style.top = `${r.top - w.top + r.height * fy}px`;
  };

  // a passage flies from its file row into its slot in the index
  const fly = (cell, row) => {
    const a = $('.e6-badge', row).getBoundingClientRect(), b = cell.getBoundingClientRect();
    cell.style.transition = 'none';
    cell.style.transform = `translate(${a.left - b.left}px, ${a.top - b.top}px) scale(.55) rotate(-14deg)`;
    cell.classList.add('in');
    void cell.offsetWidth;
    cell.style.transition = '';
    cell.style.transform = '';
  };

  // files slide from the loose list into their collections (measure, regroup, animate the difference)
  const sort = () => {
    const before = files.map((f) => f.getBoundingClientRect());
    list.classList.remove('e6-loose');
    files.forEach((f, i) => {
      const r = f.getBoundingClientRect();
      f.style.transition = 'none';
      f.style.transform = `translate(${before[i].left - r.left}px, ${before[i].top - r.top}px)`;
    });
    void list.offsetWidth;
    files.forEach((f) => { f.style.transition = ''; f.style.transform = ''; });
  };

  let events = [], next = 0, t = 0, last = 0, raf = 0, started = false, visible = false, indexed = 0;

  const reset = () => {
    scene.classList.add('e6-anim');
    scene.classList.remove('e6-dropping', 'e6-rec-on');
    list.classList.add('e6-loose');
    [...files, ...cellEls, looseH, q, ...words, ...srcBits, page, mark, hint, live, lvLine, card].forEach((el) => el.classList.remove('in'));
    files.forEach((f, i) => { f.classList.remove('reading'); f.style.transform = ''; metas[i].m.textContent = 'Waiting…'; });
    cellEls.forEach((c) => { c.classList.remove('lit'); c.style.transform = ''; });
    cells.classList.remove('scan');
    empty.classList.remove('gone');
    hiring.classList.remove('off');
    hireSw.classList.add('on');
    count.textContent = indexed = 0;
    typed.textContent = '';
    input.classList.add('empty');
    input.classList.remove('focus');
    askBtn.classList.remove('press');
    cursor.classList.remove('show', 'click');
    cursor.style.left = cursor.style.top = '';
    navAsk.classList.remove('on');
    navDocs.classList.add('on');
    lvT.textContent = '00:15';
    lvQ.textContent = liveQ;
    mori.src = sprite('mori', 'idle');
    say.textContent = 'Got some files? Drop them on me.';
    replay.hidden = true;
    t = 0; next = 0;

    const ev = [];
    const at = (time, fn) => ev.push({ time, fn });

    // 1 · three files drop into the drive
    at(0.2, () => scene.classList.add('e6-dropping'));
    const dropOrder = [...files].sort((a, b) => a.dataset.o - b.dataset.o);
    dropOrder.forEach((f, k) => at(0.5 + k * 0.45, () => {
      looseH.classList.add('in');
      f.classList.add('in');
      metas[files.indexOf(f)].m.textContent = 'Reading…';
    }));
    at(0.8, () => pose('writing', 'Plans, specs, notes. I\'ll read them all.'));
    at(1.75, () => scene.classList.remove('e6-dropping'));

    // 2 · each one splits into passages that slot into the index
    at(1.9, () => pose('thinking', 'Cutting each one into passages…'));
    dropOrder.forEach((f, k) => {
      const t0 = 1.95 + k * 0.75, i = files.indexOf(f);
      const mine = cellEls.filter((c) => +c.dataset.f === i);
      at(t0, () => f.classList.add('reading'));
      mine.forEach((c, n) => at(t0 + 0.15 + n * 0.08, () => { fly(c, f); count.textContent = ++indexed; }));
      at(t0 + 0.72, () => { f.classList.remove('reading'); metas[i].m.textContent = metas[i].done; });
    });
    at(4.3, () => { restart(cells, 'scan'); pose('idle', '15 passages, indexed on your computer.'); });

    // 3 · they sort into two collections; "Hiring" is switched off
    at(5.2, () => { sort(); pose('writing', 'Sorted into two collections.'); });
    at(6.1, () => { cursor.classList.add('show'); point(hireSw, 0.3, 0.6); });
    at(6.9, () => {
      restart(cursor, 'click');
      hireSw.classList.remove('on');
      hiring.classList.add('off');
      metas[files.indexOf($('.e6-file', hiring))].m.textContent = metas[files.indexOf($('.e6-file', hiring))].final;
      pose('thinking', 'Hiring is off. I won\'t search it.');
    });

    // 4 · a question, answered from a meeting and a page
    at(7.6, () => point(input, 0.25, 0.6));
    at(8.2, () => { input.classList.add('focus'); input.classList.remove('empty'); navDocs.classList.remove('on'); navAsk.classList.add('on'); });
    [...question].forEach((_, k) => at(8.3 + k * 0.03, () => { typed.textContent = question.slice(0, k + 1); }));
    at(8.7, () => hint.classList.add('in'));
    at(9.05, () => point(askBtn, 0.4, 0.6));
    at(9.6, () => {
      restart(cursor, 'click');
      askBtn.classList.add('press');
      q.classList.add('in');
      empty.classList.add('gone');
      typed.textContent = '';
      input.classList.add('empty');
      restart(cells, 'scan');
      pose('thinking', 'Searching meetings and documents…');
    });
    at(9.85, () => { askBtn.classList.remove('press'); input.classList.remove('focus'); cursor.classList.remove('show'); });
    at(10.2, () => $('.hit', cells).classList.add('lit'));
    words.forEach((w, k) => at(10.4 + k * 0.035, () => w.classList.add('in')));
    const aEnd = 10.4 + words.length * 0.035;
    srcBits.forEach((s, k) => at(aEnd + 0.15 + k * 0.18, () => s.classList.add('in')));
    at(aEnd + 0.55, () => { page.classList.add('in'); pose('talking', 'The plan, page 4. And your call, 02:14.'); });
    at(aEnd + 1.05, () => mark.classList.add('in'));

    // 5 · mid-call, the same page backs up a live answer
    const call = aEnd + 2.1;
    at(call, () => { scene.classList.add('e6-rec-on'); live.classList.add('in'); $('.hit', cells).classList.remove('lit'); pose('listening', 'A call! I\'m listening.'); });
    const lw = liveQ.replace(/[:?]/g, '').split(' ');
    at(call + 0.35, () => { lvLine.classList.add('in', 'prov'); lvQ.textContent = '…'; });
    lw.forEach((_, k) => at(call + 0.4 + k * 0.13, () => { lvQ.textContent = `${lw.slice(0, k + 1).join(' ')} …`; }));
    at(call + 0.6, () => { lvT.textContent = '00:16'; });
    at(call + 1.3, () => { lvLine.classList.remove('prov'); lvQ.textContent = liveQ; });
    at(call + 1.6, () => { lvT.textContent = '00:17'; });
    at(call + 1.75, () => { card.classList.add('in'); pose('talking', finalSay); });
    at(call + 2.6, () => { lvT.textContent = '00:18'; });
    at(call + 3.0, () => { replay.hidden = false; });
    events = ev.sort((x, y) => x.time - y.time);
  };

  const frame = (now) => {
    raf = 0;
    t += Math.min(0.25, (now - last) / 1000); // keeps pace on slow machines; the clock still stops offscreen
    last = now;
    while (next < events.length && events[next].time <= t) events[next++].fn();
    if (visible && next < events.length) raf = requestAnimationFrame(frame);
  };
  const play = () => {
    scene.classList.remove('e6-paused');
    if (!raf && next < events.length) { last = performance.now(); raf = requestAnimationFrame(frame); }
  };
  const pause = () => { cancelAnimationFrame(raf); raf = 0; scene.classList.add('e6-paused'); };

  reset();
  ['writing', 'thinking', 'listening', 'talking'].forEach((p) => preload(sprite('mori', p)));
  // start once ~40% of the scene (or of the screen, on tall phones) is in view; pause whenever it leaves
  new IntersectionObserver(([e]) => {
    const vh = e.rootBounds ? e.rootBounds.height : innerHeight;
    const seen = e.intersectionRect.height / Math.min(e.boundingClientRect.height, vh);
    visible = started ? e.isIntersecting : seen >= 0.4;
    if (visible) { started = true; play(); } else pause();
  }, { threshold: [0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7, 0.8, 0.9, 1] }).observe(scene);

  replay.addEventListener('click', () => { pause(); reset(); started = visible = true; play(); });
})();
